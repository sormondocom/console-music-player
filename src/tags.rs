//! User-defined keyword tags for audio tracks.
//!
//! Tags are stored as JSON at the same config directory as the main config:
//!   Windows: %APPDATA%\console-music-player\tags.json

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TagStore {
    /// Map from canonical path string → alphabetically-sorted tag set.
    entries: HashMap<String, BTreeSet<String>>,
}

impl TagStore {
    pub fn load() -> Self {
        let path = match store_path() {
            Some(p) => p,
            None => return Self::default(),
        };
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                warn!("Tag store parse error: {e} — starting empty.");
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                warn!("Could not read tag store: {e}");
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let path = match store_path() {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!("Could not create config dir: {e}");
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!("Could not write tag store: {e}");
                } else {
                    info!("Tag store saved to {}", path.display());
                }
            }
            Err(e) => warn!("Could not serialize tag store: {e}"),
        }
    }

    /// Return the sorted tags for `path`, or an empty vec.
    pub fn tags_for(&self, path: &Path) -> Vec<String> {
        let key = canonical(path);
        self.entries
            .get(&key)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Replace the tags for `path` with `tags` (normalised: lowercase, trimmed, deduped).
    pub fn set_tags(&mut self, path: &Path, tags: Vec<String>) {
        let key = canonical(path);
        let normalised: BTreeSet<String> = tags
            .into_iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if normalised.is_empty() {
            self.entries.remove(&key);
        } else {
            self.entries.insert(key, normalised);
        }
    }

    /// All distinct tags across all tracks, sorted.
    pub fn all_tags(&self) -> Vec<String> {
        let mut set: BTreeSet<&str> = BTreeSet::new();
        for tags in self.entries.values() {
            for t in tags {
                set.insert(t.as_str());
            }
        }
        set.into_iter().map(str::to_owned).collect()
    }
}

fn canonical(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn store_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA").ok().map(PathBuf::from)?
    } else {
        let home = std::env::var("HOME").ok().map(PathBuf::from)?;
        home.join(".config")
    };
    Some(base.join("console-music-player").join("tags.json"))
}
