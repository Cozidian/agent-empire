//! A sector joins worlds around a product, wherever their folders live.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sector {
    pub id: u64,
    pub name: String,
    pub worlds: Vec<String>,
}

impl Sector {
    pub fn contains(&self, world: &str, host: Option<&str>) -> bool {
        self.worlds.iter().any(|w| w == world || Some(w.as_str()) == host)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    next: u64,
    pub sectors: Vec<Sector>,
}

impl Book {
    fn name(&self, name: &str, except: Option<u64>) -> Result<String, String> {
        if name.chars().any(char::is_control) { return Err("Use a single-line sector name".into()); }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 48 { return Err("A sector name needs 1–48 characters".into()); }
        if self.sectors.iter().any(|s| Some(s.id) != except && s.name.to_lowercase() == name.to_lowercase()) {
            return Err("A sector with that name already exists".into());
        }
        Ok(name.into())
    }

    pub fn create(&mut self, name: &str) -> Result<u64, String> {
        let name = self.name(name, None)?;
        let id = self.next.max(self.sectors.iter().map(|s| s.id).max().unwrap_or(0)).checked_add(1).ok_or("No sector ids left")?;
        self.next = id;
        self.sectors.push(Sector { id, name, worlds: vec![] });
        Ok(id)
    }

    pub fn rename(&mut self, id: u64, name: &str) -> Result<(), String> {
        let name = self.name(name, Some(id))?;
        self.sectors.iter_mut().find(|s| s.id == id).ok_or("This sector is no longer available")?.name = name;
        Ok(())
    }

    pub fn toggle(&mut self, id: u64, world: &str) -> Result<(), String> {
        if !std::path::Path::new(world).is_absolute() || world.chars().any(char::is_control) { return Err("Choose an absolute world path".into()); }
        let s = self.sectors.iter_mut().find(|s| s.id == id).ok_or("This sector is no longer available")?;
        if s.worlds.iter().any(|w| w == world) { s.worlds.retain(|w| w != world); }
        else { s.worlds.push(world.into()); s.worlds.sort(); }
        Ok(())
    }

    pub fn remove(&mut self, id: u64) -> Result<(), String> {
        if !self.sectors.iter().any(|s| s.id == id) { return Err("This sector is no longer available".into()); }
        self.sectors.retain(|s| s.id != id);
        Ok(())
    }
}


pub fn path() -> std::path::PathBuf { crate::daemon::dirs_state().join("sectors.json") }

pub fn edit(path: &std::path::Path, change: impl FnOnce(&mut Book)) -> std::io::Result<Book> { crate::state::edit(path, change) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sectors_join_worlds_across_directories_without_exclusive_ownership() {
        let mut b = Book::default();
        let id = b.create(" Kiplat ").unwrap();
        for world in ["/work/runtime", "/other/frontend", "/services/bifrost"] { b.toggle(id, world).unwrap(); }
        assert_eq!(b.sectors[0].name, "Kiplat");
        assert_eq!(b.sectors[0].worlds.len(), 3);
        assert!(b.sectors[0].contains("/moons/frontend-fix", Some("/other/frontend")));
        assert!(!b.sectors[0].contains("/other/frontend-extra", None));
        let shared = b.create("Platform").unwrap();
        b.toggle(shared, "/services/bifrost").unwrap();
        assert!(b.sectors.iter().all(|s| s.contains("/services/bifrost", None)));
        b.toggle(id, "/services/bifrost").unwrap();
        assert!(!b.sectors[0].contains("/services/bifrost", None));
        assert!(b.sectors[1].contains("/services/bifrost", None));
    }

    #[test]
    fn names_are_validated_and_failed_edits_preserve_the_book() {
        let mut b = Book::default();
        let id = b.create("Kiplat").unwrap();
        for name in ["", "   ", "kIpLaT", "bad\nname"] { assert!(b.create(name).is_err()); }
        let other = b.create("Research").unwrap();
        let before = b.clone();
        assert!(b.rename(other, " Kiplat ").is_err());
        assert_eq!(b, before);
        b.rename(id, "KIPLAT").unwrap();
        assert_eq!(b.sectors[0].id, id);
        assert_eq!(b.sectors[0].name, "KIPLAT");
    }

    #[test]
    fn dissolving_a_sector_does_not_reuse_its_identity_or_touch_other_groups() {
        let mut b = Book::default();
        let id = b.create("Kiplat").unwrap();
        b.toggle(id, "/world").unwrap();
        b.remove(id).unwrap();
        assert!(b.sectors.is_empty());
        assert_ne!(b.create("Kiplat").unwrap(), id);
        let before = b.clone();
        assert!(b.toggle(id, "/world").is_err());
        assert_eq!(b, before);
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    #[test]
    fn simultaneous_membership_updates_survive_reload_and_corruption_is_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("empire-sector-storage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sectors.json");
        edit(&path, |b| { b.create("Kiplat").unwrap(); }).unwrap();
        let threads: Vec<_> = (0..6).map(|i| {
            let path = path.clone();
            std::thread::spawn(move || edit(&path, |b| b.toggle(1, &format!("/world-{i}")).unwrap()).unwrap())
        }).collect();
        for t in threads { t.join().unwrap(); }
        let saved = edit(&path, |_| {}).unwrap();
        assert_eq!(saved.sectors[0].worlds, (0..6).map(|i| format!("/world-{i}")).collect::<Vec<_>>());
        std::fs::write(&path, "broken data").unwrap();
        assert!(edit(&path, |b| { b.remove(1).unwrap(); }).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken data");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
