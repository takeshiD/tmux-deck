use std::collections::HashMap;
use std::path::PathBuf;

use directories::ProjectDirs;
use tracing::{debug, warn};

// =============================================================================
// GroupStore — tmux-deck-side session grouping
// =============================================================================
//
// Grouping is a tmux-deck concept, not a tmux one: tmux has its own "session
// groups" (shared windows), but here a group is purely an organisational label
// the user attaches to a session inside the deck. The mapping
// `session_name -> group_name` is persisted to a small TSV file in the user's
// config directory so groups survive restarts.
//
// The file format is intentionally trivial (one `session\tgroup` pair per line)
// to avoid pulling in a serialization dependency — tmux-deck ships as a single
// binary with no runtime dependencies and we keep it that way.

#[derive(Debug, Default)]
pub struct GroupStore {
    /// session name -> group name
    assignments: HashMap<String, String>,
    /// Where the store is persisted. `None` when no config dir could be
    /// resolved; the store then behaves as an in-memory-only best effort.
    path: Option<PathBuf>,
}

impl GroupStore {
    /// Load the store from the user's config directory. Missing or unreadable
    /// files yield an empty store — grouping is best-effort and must never
    /// prevent the app from starting.
    pub fn load() -> Self {
        let path = Self::default_path();
        let mut assignments = HashMap::new();
        if let Some(p) = path.as_ref()
            && let Ok(contents) = std::fs::read_to_string(p)
        {
            for line in contents.lines() {
                if let Some((session, group)) = line.split_once('\t') {
                    let session = session.trim();
                    let group = group.trim();
                    if !session.is_empty() && !group.is_empty() {
                        assignments.insert(session.to_string(), group.to_string());
                    }
                }
            }
            debug!("loaded {} group assignment(s)", assignments.len());
        }
        Self { assignments, path }
    }

    fn default_path() -> Option<PathBuf> {
        let dirs = ProjectDirs::from("dev", "tkcd", "tmux-deck")?;
        Some(dirs.config_dir().join("groups.tsv"))
    }

    /// Group a session belongs to, if any.
    pub fn group_of(&self, session: &str) -> Option<String> {
        self.assignments.get(session).cloned()
    }

    /// Assign `session` to `group`, or remove it from any group when `group`
    /// is `None` or empty. Persists immediately (best effort).
    pub fn set(&mut self, session: &str, group: Option<&str>) {
        match group.map(str::trim).filter(|g| !g.is_empty()) {
            Some(g) => {
                self.assignments.insert(session.to_string(), g.to_string());
            }
            None => {
                self.assignments.remove(session);
            }
        }
        self.save();
    }

    /// Carry a session's group across a rename so the assignment is not lost
    /// when the underlying tmux session changes name.
    pub fn rename_session(&mut self, old_name: &str, new_name: &str) {
        if let Some(group) = self.assignments.remove(old_name) {
            self.assignments.insert(new_name.to_string(), group);
            self.save();
        }
    }

    /// Drop a killed session's assignment so the store does not accumulate
    /// entries for sessions that no longer exist.
    pub fn forget(&mut self, session: &str) {
        if self.assignments.remove(session).is_some() {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            warn!("failed to create config dir for group store: {e}");
            return;
        }
        // Tabs/newlines in a session name would corrupt the line format; tmux
        // session names cannot contain them in practice, but guard anyway.
        let mut out = String::new();
        for (session, group) in &self.assignments {
            if session.contains(['\t', '\n']) || group.contains(['\t', '\n']) {
                continue;
            }
            out.push_str(session);
            out.push('\t');
            out.push_str(group);
            out.push('\n');
        }
        if let Err(e) = std::fs::write(path, out) {
            warn!("failed to write group store: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_query() {
        let mut store = GroupStore::default();
        assert_eq!(store.group_of("a"), None);
        store.set("a", Some("work"));
        assert_eq!(store.group_of("a"), Some("work".to_string()));
        // Empty / whitespace group removes the assignment.
        store.set("a", Some("  "));
        assert_eq!(store.group_of("a"), None);
        store.set("a", Some("work"));
        store.set("a", None);
        assert_eq!(store.group_of("a"), None);
    }

    #[test]
    fn rename_preserves_group() {
        let mut store = GroupStore::default();
        store.set("old", Some("work"));
        store.rename_session("old", "new");
        assert_eq!(store.group_of("old"), None);
        assert_eq!(store.group_of("new"), Some("work".to_string()));
    }

    #[test]
    fn forget_removes() {
        let mut store = GroupStore::default();
        store.set("a", Some("work"));
        store.forget("a");
        assert_eq!(store.group_of("a"), None);
    }
}
