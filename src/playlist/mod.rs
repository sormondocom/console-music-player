//! Persistent playlists stored as JSON files.
//!
//! Location: `{config_dir}/console-music-player/playlists/{sanitized_name}.json`

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::{AppError, Result};

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    /// Display name (the user-visible string).
    pub name: String,
    /// ISO date the playlist was created: "YYYY-MM-DD".
    pub created_at: String,
    /// Absolute paths to audio files in order.
    pub tracks: Vec<PathBuf>,
}

impl Playlist {
    pub fn new(name: impl Into<String>, tracks: Vec<PathBuf>) -> Self {
        Self {
            name: name.into(),
            created_at: today_str(),
            tracks,
        }
    }

    /// Merge `other`'s tracks into this playlist (deduped by path).
    pub fn merge_from(&mut self, other: &Playlist) {
        for path in &other.tracks {
            if !self.tracks.contains(path) {
                self.tracks.push(path.clone());
            }
        }
    }

    /// Persist to `{playlists_dir}/{sanitized_name}.json`.
    pub fn save(&self) -> Result<()> {
        let dir = playlists_dir()?;
        fs::create_dir_all(&dir)?;
        let path = dir.join(sanitize_filename(&self.name) + ".json");
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Metadata(e.to_string()))?;
        fs::write(&path, json)?;
        info!("Playlist saved: {}", path.display());
        Ok(())
    }

    /// Load a playlist by display name.
    pub fn load(name: &str) -> Result<Self> {
        let path = playlists_dir()?.join(sanitize_filename(name) + ".json");
        let json = fs::read_to_string(&path)?;
        serde_json::from_str(&json).map_err(|e| AppError::Metadata(e.to_string()))
    }

    /// Delete a playlist by display name.
    pub fn delete(name: &str) -> Result<()> {
        let path = playlists_dir()?.join(sanitize_filename(name) + ".json");
        fs::remove_file(path)?;
        Ok(())
    }

    /// Check whether a playlist with this name already exists on disk.
    pub fn exists(name: &str) -> bool {
        playlists_dir()
            .map(|d| d.join(sanitize_filename(name) + ".json").exists())
            .unwrap_or(false)
    }

    /// Return all saved playlist names (sorted alphabetically).
    pub fn list_all() -> Vec<String> {
        let Ok(dir) = playlists_dir() else { return Vec::new() };
        let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };

        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x == "json")
                    .unwrap_or(false)
            })
            .filter_map(|e| {
                // Read the JSON to get the display name (not the filename).
                fs::read_to_string(e.path())
                    .ok()
                    .and_then(|s| serde_json::from_str::<Playlist>(&s).ok())
                    .map(|p| p.name)
            })
            .collect();

        names.sort();
        names
    }
}

// ---------------------------------------------------------------------------
// Conflict resolution context
// ---------------------------------------------------------------------------

/// Holds the information needed to resolve a save-name collision.
#[derive(Debug, Clone)]
pub struct ConflictCtx {
    /// The name the user typed.
    pub name: String,
    /// Tracks from the new (unsaved) playlist.
    pub new_tracks: Vec<PathBuf>,
    /// Tracks already in the on-disk playlist.
    pub existing_tracks: Vec<PathBuf>,
}

impl ConflictCtx {
    /// Overwrite: save `new_tracks` under the original name.
    pub fn resolve_overwrite(&self) -> Result<()> {
        Playlist::new(&self.name, self.new_tracks.clone()).save()
    }

    /// New dated: merge tracks and save under "{name} ({date})".
    pub fn resolve_new_dated(&self) -> Result<String> {
        let dated_name = format!("{} ({})", self.name, today_str());
        let mut pl = Playlist::new(&dated_name, self.existing_tracks.clone());
        pl.merge_from(&Playlist::new("_tmp_", self.new_tracks.clone()));
        pl.save()?;
        Ok(dated_name)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn playlists_dir() -> Result<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .map_err(|_| AppError::Io(std::io::Error::other("APPDATA not set")))?
    } else {
        let home = std::env::var("HOME")
            .map_err(|_| AppError::Io(std::io::Error::other("HOME not set")))?;
        PathBuf::from(home).join(".config")
    };
    Ok(base.join("console-music-player").join("playlists"))
}

/// Replace characters unsafe for filenames with underscores.
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Current date as "YYYY-MM-DD" without external crate dependencies.
pub fn today_str() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86400;

    // Civil date from days-since-epoch (Hinnant algorithm)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
