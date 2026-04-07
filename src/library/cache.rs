//! Scan cache — persists track metadata between runs so that unchanged files
//! are not re-parsed by lofty on every startup.
//!
//! ## Key design
//!
//! Each file is identified by its **absolute path + mtime (seconds) + size**.
//! If both mtime and size match the cached value, the file content has not
//! changed and the stored metadata is returned directly — no lofty call needed.
//!
//! Files that are new, modified, or whose cache entry is absent fall through
//! to the full `read_metadata` path.  Entries for files that no longer exist
//! are simply not included when the cache is re-saved, so the file stays lean.
//!
//! ## Format
//!
//! JSON stored at the same location as `config.json`:
//!   Windows : `%APPDATA%\console-music-player\scan_cache.json`
//!   Linux   : `$HOME/.config/console-music-player/scan_cache.json`
//!
//! A top-level `"version"` field is checked on load; a mismatch causes a
//! silent cache invalidation so format changes never break the app.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Bump this whenever the on-disk format changes incompatibly.
const CACHE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// On-disk types
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<PathBuf, CacheEntry>,
}

/// Everything needed to reconstruct a [`super::Track`] without touching the file.
#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    /// File modification time in whole seconds since the Unix epoch.
    pub mtime_secs: u64,
    /// File size in bytes at the time of last scan.
    pub file_size: u64,

    // --- track fields ---
    pub title:          String,
    pub artist:         String,
    pub album:          String,
    pub year:           Option<u32>,
    pub duration_secs:  Option<u32>,
    pub bitrate_kbps:   Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels:       Option<u8>,
}

// ---------------------------------------------------------------------------
// ScanCache
// ---------------------------------------------------------------------------

/// In-memory view of the scan cache.  Call [`ScanCache::load`] at startup,
/// use [`ScanCache::get`] during scanning, [`ScanCache::insert`] for cache
/// misses, then [`ScanCache::save`] when the scan is complete.
pub struct ScanCache {
    entries: HashMap<PathBuf, CacheEntry>,
    /// Tracks which paths were actually referenced in the current scan so we
    /// can drop stale entries when saving.
    referenced: std::collections::HashSet<PathBuf>,
}

impl ScanCache {
    /// Load the cache from disk.  Returns an empty cache on any error so the
    /// scan always proceeds correctly even if the file is missing or corrupt.
    pub fn load() -> Self {
        let entries = cache_path()
            .and_then(|p| fs::read_to_string(&p).ok())
            .and_then(|json| serde_json::from_str::<CacheFile>(&json).ok())
            .and_then(|cf| {
                if cf.version == CACHE_VERSION {
                    Some(cf.entries)
                } else {
                    info!("Scan cache version mismatch — invalidating.");
                    None
                }
            })
            .unwrap_or_default();

        let n = entries.len();
        if n > 0 {
            info!("Scan cache loaded: {n} entries.");
        }

        Self {
            entries,
            referenced: std::collections::HashSet::new(),
        }
    }

    /// Look up a file by path.  Returns the cached entry **only** if mtime and
    /// size both match the values on disk (passed in by the caller after a
    /// `stat` call), meaning the file content is guaranteed unchanged.
    pub fn get(&mut self, path: &Path, mtime_secs: u64, file_size: u64) -> Option<&CacheEntry> {
        let entry = self.entries.get(path)?;
        if entry.mtime_secs == mtime_secs && entry.file_size == file_size {
            self.referenced.insert(path.to_path_buf());
            Some(entry)
        } else {
            None
        }
    }

    /// Store a freshly-parsed entry.  Call this after every cache miss.
    pub fn insert(&mut self, path: PathBuf, entry: CacheEntry) {
        self.referenced.insert(path.clone());
        self.entries.insert(path, entry);
    }

    /// Persist the cache to disk, dropping entries for files that were not
    /// seen in the current scan (deleted or no-longer-scanned files).
    pub fn save(&self) {
        let Some(path) = cache_path() else { return };

        // Keep only entries that were actually referenced this scan.
        let entries: HashMap<PathBuf, CacheEntry> = self
            .entries
            .iter()
            .filter(|(k, _)| self.referenced.contains(*k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let cf = CacheFile { version: CACHE_VERSION, entries };

        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                warn!("Could not create cache dir {}: {e}", parent.display());
                return;
            }
        }

        match serde_json::to_string(&cf) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    warn!("Could not write scan cache to {}: {e}", path.display());
                } else {
                    info!("Scan cache saved: {} entries.", cf.entries.len());
                }
            }
            Err(e) => warn!("Could not serialise scan cache: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the mtime of `metadata` as whole seconds since the Unix epoch.
/// Returns `0` on any error (which will force a cache miss, the safe default).
pub fn mtime_secs(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA").ok().map(PathBuf::from)?
    } else {
        let home = std::env::var("HOME").ok().map(PathBuf::from)?;
        home.join(".config")
    };
    Some(base.join("console-music-player").join("scan_cache.json"))
}
