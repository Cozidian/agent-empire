//! Named, directional relationships between worlds, charted by the player.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lane { pub id: u64, pub from: String, pub to: String, pub label: String }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    next: u64,
    pub lanes: Vec<Lane>,
}

impl Book {
    pub fn save(&mut self, id: Option<u64>, from: &str, to: &str, label: &str) -> Result<u64, String> {
        for p in [from, to] {
            if !std::path::Path::new(p).is_absolute() || p.chars().any(char::is_control) { return Err("Choose an absolute world path".into()); }
        }
        if from == to { return Err("Choose two different worlds".into()); }
        if label.chars().any(char::is_control) { return Err("Use a single-line relationship label".into()); }
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 48 { return Err("A relationship label needs 1–48 characters".into()); }
        if id.is_some_and(|id| !self.lanes.iter().any(|l| l.id == id)) { return Err("This hyperlane is no longer available".into()); }
        if self.lanes.iter().any(|l| Some(l.id) != id && l.from == from && l.to == to && l.label.to_lowercase() == label.to_lowercase()) {
            return Err("That relationship already exists in this direction".into());
        }
        let key = match id {
            Some(id) => id,
            None => self.next.max(self.lanes.iter().map(|l| l.id).max().unwrap_or(0)).checked_add(1).ok_or("No hyperlane ids left")?,
        };
        let lane = Lane { id: key, from: from.into(), to: to.into(), label: label.into() };
        if let Some(old) = self.lanes.iter_mut().find(|l| l.id == key) { *old = lane; }
        else { self.next = key; self.lanes.push(lane); }
        Ok(key)
    }

    pub fn remove(&mut self, id: u64) -> Result<(), String> {
        if !self.lanes.iter().any(|l| l.id == id) { return Err("This hyperlane is no longer available".into()); }
        self.lanes.retain(|l| l.id != id);
        Ok(())
    }
}

pub fn path() -> std::path::PathBuf { crate::daemon::dirs_state().join("hyperlanes.json") }
pub fn edit(path: &std::path::Path, change: impl FnOnce(&mut Book)) -> std::io::Result<Book> { crate::state::edit(path, change) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationships_keep_direction_and_identity_when_edited() {
        let mut b = Book::default();
        let id = b.save(None, "/product/frontend", "/services/gateway", " calls ").unwrap();
        let reverse = b.save(None, "/services/gateway", "/product/frontend", "calls").unwrap();
        assert_ne!(id, reverse);
        b.save(Some(id), "/product/frontend", "/services/gateway", "depends on").unwrap();
        assert_eq!(b.lanes[0], Lane { id, from: "/product/frontend".into(), to: "/services/gateway".into(), label: "depends on".into() });
        b.remove(id).unwrap();
        assert_eq!(b.lanes[0].id, reverse);
        assert!(b.save(None, "/a", "/b", "calls").unwrap() > reverse);
    }

    #[test]
    fn invalid_or_duplicate_links_leave_the_book_untouched() {
        let mut b = Book::default();
        b.save(None, "/a", "/b", "calls").unwrap();
        let before = b.clone();
        for (id, from, to, label) in [(None, "/a", "/a", "calls"), (None, "relative", "/b", "calls"), (None, "/a", "/b", " CALLS "),
            (None, "/a", "/b", "\n"), (None, "/a", "/b", ""), (Some(99), "/a", "/b", "new"), (None, "/bad\npath", "/b", "calls")] {
            assert!(b.save(id, from, to, label).is_err());
            assert_eq!(b, before);
        }
        assert!(b.save(None, "/a", "/b", &"x".repeat(49)).is_err());
        assert_eq!(b, before);
        assert!(b.remove(99).is_err());
    }

    #[test]
    fn concurrent_links_survive_reload_and_corruption_is_preserved() {
        let dir = std::env::temp_dir().join(format!("empire-hyperlane-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hyperlanes.json");
        let threads: Vec<_> = (0..6).map(|i| {
            let path = path.clone();
            std::thread::spawn(move || edit(&path, |b| { b.save(None, &format!("/source-{i}"), "/target", "calls").unwrap(); }).unwrap())
        }).collect();
        for t in threads { t.join().unwrap(); }
        let b = edit(&path, |_| {}).unwrap();
        for i in 0..6 { assert!(b.lanes.iter().any(|l| l.from == format!("/source-{i}") && l.to == "/target" && l.label == "calls")); }
        std::fs::write(&path, "broken").unwrap();
        assert!(edit(&path, |b| { b.remove(1).unwrap(); }).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
