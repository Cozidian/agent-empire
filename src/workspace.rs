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
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

pub fn path() -> PathBuf { crate::daemon::dirs_state().join("workspace.json") }

impl Config {
    pub fn load() -> std::io::Result<Self> {
        match std::fs::read(path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                let root = [PathBuf::from("/home/code"), home.join("code")].into_iter().find(|p| p.is_dir()).unwrap_or_default();
                Ok(Self { root: root.to_string_lossy().into_owned(), ..Default::default() })
            }
            Err(e) => Err(e),
        }
    }
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path())
    }
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        // an explicit empty workspace must differ from an absent file (which chooses a default root)
        crate::state::edit::<Option<Self>>(path, |c| *c = Some(self.clone())).map(|_| ())
    }
    pub fn projects(&self) -> Vec<String> {
        let mut paths: Vec<_> = self.discovery_roots().into_iter().flat_map(|p| discover(Path::new(p))).collect();
        paths.extend(self.folders.iter().filter_map(|p| std::fs::canonicalize(p).ok()).filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()));
        paths.retain(|p| !self.hidden.contains(p));
        paths.sort(); paths.dedup(); paths
    }
    pub fn discovery_roots(&self) -> Vec<&str> {
        let mut roots: Vec<_> = std::iter::once(self.root.as_str()).chain(self.roots.iter().map(String::as_str)).filter(|p| !p.is_empty()).collect();
        roots.sort(); roots.dedup(); roots
    }
    pub fn sources(&self) -> Vec<(bool, String)> {
        self.discovery_roots().into_iter().map(|p| (true, p.into())).chain(self.folders.iter().map(|p| (false, p.clone()))).collect()
    }
    pub fn includes(&self, path: &str) -> bool {
        self.folders.iter().any(|p| p == path) || self.discovery_roots().iter().any(|p| Path::new(path).starts_with(p))
    }
    pub fn hide(&mut self, key: &str) {
        self.folders.retain(|p| p != key);
        if !self.hidden.iter().any(|p| p == key) { self.hidden.push(key.into()); }
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
    fn removing_the_default_root_before_discovery_persists_an_empty_workspace() {
        let dir = std::env::temp_dir().join(format!("empire-empty-workspace-{}", std::process::id()));
        let path = dir.join("workspace.json");
        Config::default().save_to(&path).unwrap();
        let c: Config = serde_json::from_slice(&std::fs::read(&path).expect("an empty workspace must not fall back to the default root on restart")).unwrap();
        assert!(c.discovery_roots().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn multiple_roots_and_removed_projects_survive_reload() {
        let dir = std::env::temp_dir().join(format!("empire-roots-{}", std::process::id()));
        for p in ["first/team/api/.git", "second/tools/web/.git", "second/tools/hidden/.git"] { std::fs::create_dir_all(dir.join(p)).unwrap(); }
        let api = dir.join("first/team/api").to_string_lossy().into_owned();
        let web = dir.join("second/tools/web").to_string_lossy().into_owned();
        let hidden = dir.join("second/tools/hidden").to_string_lossy().into_owned();
        let c: Config = serde_json::from_value(serde_json::json!({
            "root": dir.join("first"), "roots": [dir.join("second"), dir.join("first/team")], "folders": [], "hidden": [hidden]
        })).unwrap();
        let back: Config = serde_json::from_slice(&serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(back.projects(), vec![api, web]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folder_sectors_keep_their_identity_and_distinguish_external_folders() {
        let c = Config { root: "/code".into(), ..Default::default() };
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
