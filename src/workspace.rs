//! The folders that make up the galaxy.
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

pub const HOME: &str = "empire://Home";
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub folders: Vec<String>,
}

pub fn path() -> PathBuf { crate::daemon::dirs_state().join("workspace.json") }

impl Config {
    pub fn load() -> std::io::Result<Self> {
        match std::fs::read(path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                let root = [PathBuf::from("/home/code"), home.join("code")].into_iter().find(|p| p.is_dir()).unwrap_or_default();
                Ok(Self { root: root.to_string_lossy().into_owned(), folders: vec![] })
            }
            Err(e) => Err(e),
        }
    }
    pub fn save(&self) -> std::io::Result<()> {
        crate::state::edit::<Self>(&path(), |c| *c = self.clone()).map(|_| ())
    }
    pub fn projects(&self) -> Vec<String> {
        let mut paths = if self.root.is_empty() { vec![] } else { discover(Path::new(&self.root)) };
        paths.extend(self.folders.iter().filter_map(|p| std::fs::canonicalize(p).ok()).filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()));
        paths.sort(); paths.dedup(); paths
    }
    pub fn sectors(&self, projects: &[String]) -> crate::sector::Book {
        let mut groups = std::collections::BTreeMap::<String, Vec<String>>::new();
        for p in projects {
            let parent = Path::new(p).parent().unwrap_or(Path::new(p));
            let name = parent.strip_prefix(&self.root).ok().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| if parent == Path::new(&self.root) { parent.file_name().unwrap_or_default().to_string_lossy().into_owned() } else { parent.to_string_lossy().into_owned() });
            groups.entry(name).or_default().push(p.clone());
        }
        let mut b = crate::sector::Book::default();
        b.sectors = groups.into_iter().map(|(name, worlds)| { let id = name.bytes().fold(14695981039346656037u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211)); crate::sector::Sector { id, name, worlds } }).collect(); b
    }
}

pub fn discover(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(root) = root.canonicalize() else { return found };
    let mut todo = vec![root];
    while let Some(p) = todo.pop() {
        if p.join(".git").exists() { found.push(p.to_string_lossy().into_owned()); continue; }
        let Ok(entries) = std::fs::read_dir(&p) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || matches!(&*name, "node_modules" | "target" | "vendor") { continue; }
            if entry.file_type().is_ok_and(|t| t.is_dir()) { todo.push(entry.path()); }
        }
    }
    found.sort(); found
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_sectors_keep_their_identity_and_distinguish_external_folders() {
        let c = Config { root: "/code".into(), folders: vec![] };
        let p = vec!["/code/team/api".into(), "/outside/team/web".into()];
        let old = c.sectors(&p); assert_eq!(old.sectors.len(), 2);
        let mut more = p; more.push("/code/aaa/other".into());
        let next = c.sectors(&more);
        for g in old.sectors { assert_eq!(next.sectors.iter().find(|s| s.name == g.name).unwrap().id, g.id); }
    }

    #[test]
    fn discovery_finds_repos_and_worktrees_without_entering_repos_or_symlinks() {
        let root = std::env::temp_dir().join(format!("empire-discovery-{}", std::process::id()));
        std::fs::create_dir_all(root.join("team/api/.git")).unwrap();
        std::fs::create_dir_all(root.join("team/api/vendor/nested/.git")).unwrap();
        std::fs::create_dir_all(root.join("team/web")).unwrap();
        std::fs::write(root.join("team/web/.git"), "gitdir: /elsewhere").unwrap();
        std::fs::create_dir_all(root.join("node_modules/package/.git")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        assert_eq!(discover(&root), vec![root.join("team/api").to_string_lossy().into_owned(), root.join("team/web").to_string_lossy().into_owned()]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
