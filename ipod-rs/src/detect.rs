//! iPod volume detection, firmware probing, upload, and database repair.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::itunesdb;
use crate::itunessd;
use crate::{IpodError, IpodTrack, Result};

// ---------------------------------------------------------------------------
// IpodKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpodKind {
    /// Classic / Mini / Nano — uses iTunesDB + Fxx folder layout.
    Classic,
    /// Shuffle (1st–3rd gen) — uses iTunesSD + flat Music/ layout.
    Shuffle,
}

impl fmt::Display for IpodKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpodKind::Classic => write!(f, "iPod"),
            IpodKind::Shuffle => write!(f, "iPod shuffle"),
        }
    }
}

// ---------------------------------------------------------------------------
// FirmwareInfo — read on detect, exposed to callers
// ---------------------------------------------------------------------------

/// Hardware and database information read from the device on detection.
///
/// Gathered from `iPod_Control/Device/SysInfo` and the iTunesDB header.
#[derive(Debug, Clone, Default)]
pub struct FirmwareInfo {
    /// Raw model identifier from SysInfo (e.g. `"MA446LL"`).
    pub model_str: String,
    /// Human-readable hardware version string (e.g. `"0x00000004"`).
    pub hw_version_str: String,
    /// iTunesDB format version (0–12+).  0 means not readable / Shuffle.
    pub db_version: u32,
    /// Derived human-readable generation label.
    pub generation: String,
}

impl fmt::Display for FirmwareInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.generation.is_empty() {
            write!(f, "DB v{}", self.db_version)
        } else {
            write!(f, "{} (DB v{})", self.generation, self.db_version)
        }
    }
}

// ---------------------------------------------------------------------------
// Health scan results
// ---------------------------------------------------------------------------

/// An audio file found on the device with no matching `mhit` in iTunesDB.
#[derive(Debug, Clone)]
pub struct OrphanedFile {
    /// iPod-relative path (forward slashes), e.g. `/iPod_Control/Music/F00/AAAA.mp3`.
    pub ipod_rel_path: String,
    /// Absolute path on the mounted volume.
    pub abs_path: PathBuf,
}

/// A `mhit` record whose `track_id` is absent from the master playlist `mhyp`.
///
/// The file was added to the track list but the playlist link was never written,
/// so it doesn't appear under Songs on the device.
#[derive(Debug, Clone)]
pub struct IncompleteEntry {
    pub track_id: u32,
    pub ipod_rel_path: String,
    pub title: String,
}

/// Outcome of a single track upload.
#[derive(Debug)]
pub struct UploadResult {
    /// iPod-relative path where the file was stored.
    pub ipod_rel_path: String,
    /// Whether the device database (iTunesDB / iTunesSD) was updated.
    /// If `false`, the file was copied but won't appear in the device menus.
    pub db_updated: bool,
    /// Step-by-step log of every action taken during the upload.
    /// Always populated — use this to show the user what happened.
    pub log: Vec<String>,
}

/// A track found on a connected iPod device.
///
/// May come from iTunesDB (full metadata) or a filesystem scan (filename only).
#[derive(Debug, Clone)]
pub struct DeviceTrackEntry {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub ipod_rel_path: String,
    pub duration_ms: u32,
    /// `true` = read from iTunesDB; `false` = discovered by filesystem scan.
    pub from_db: bool,
}

/// Results of [`IpodDevice::scan_health`].
#[derive(Debug, Default)]
pub struct DeviceScanResult {
    /// Files on the device filesystem not registered in the database.
    pub orphaned_files: Vec<OrphanedFile>,
    /// Database tracks missing from the master Songs playlist.
    pub incomplete_entries: Vec<IncompleteEntry>,
}

impl DeviceScanResult {
    pub fn is_healthy(&self) -> bool {
        self.orphaned_files.is_empty() && self.incomplete_entries.is_empty()
    }

    pub fn issue_count(&self) -> usize {
        self.orphaned_files.len() + self.incomplete_entries.len()
    }
}

// ---------------------------------------------------------------------------
// IpodDevice
// ---------------------------------------------------------------------------

/// Represents a single connected iPod (USB Mass Storage).
#[derive(Debug, Clone)]
pub struct IpodDevice {
    /// Root mount point (e.g. `E:\` on Windows, `/media/ipod` on Linux).
    pub root: PathBuf,
    pub kind: IpodKind,
    pub firmware: FirmwareInfo,
    label: String,
}

impl IpodDevice {
    /// Scan all mounted volumes and return every iPod found.
    ///
    /// Firmware information is probed during detection so it's immediately
    /// available without an additional call.
    pub fn detect() -> Vec<IpodDevice> {
        find_ipod_volumes()
            .into_iter()
            .filter_map(|root| build_device(root).ok())
            .collect()
    }

    /// Human-readable device label (volume name or drive letter).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Display name including kind and label, e.g. `"iPod (E:)"`.
    pub fn display_name(&self) -> String {
        format!("{} ({})", self.kind, self.label)
    }

    /// Best-effort free space in bytes. `None` if unavailable.
    pub fn free_space(&self) -> Option<u64> {
        free_space_bytes(&self.root)
    }

    // --- paths ---

    pub fn music_dir(&self) -> PathBuf {
        self.root.join("iPod_Control").join("Music")
    }

    /// Locate `iTunesDB` on this device.
    ///
    /// Tries a ranked list of known locations first; if none exist, performs a
    /// recursive scan of the whole device looking for the filename
    /// (case-insensitive). Returns `None` if the file cannot be found at all —
    /// e.g. on a freshly restored iPod that has never been synced.
    pub fn find_itunesdb(&self) -> Option<PathBuf> {
        find_db_file(&self.root, "iTunesDB")
    }

    /// Locate `iTunesSD` on this device (Shuffle).
    pub fn find_itunessd(&self) -> Option<PathBuf> {
        find_db_file(&self.root, "iTunesSD")
    }

    /// Create a fresh iTunesDB at the standard location if one does not exist.
    ///
    /// Safe to call on any device; returns `Ok(path)` with the location of the
    /// newly created (or already-existing) database, or an error if the device
    /// root is not writable.
    ///
    /// After this call `find_itunesdb()` is guaranteed to succeed.
    pub fn init_database(&self) -> Result<PathBuf> {
        if self.kind != IpodKind::Classic {
            return Err(IpodError::Database(
                "Database initialisation only applies to Classic/Nano/Mini iPods".into(),
            ));
        }
        let itunes_dir = self.root.join("iPod_Control").join("iTunes");
        let db_path = itunes_dir.join("iTunesDB");
        if db_path.exists() {
            info!("iTunesDB already exists at {}", db_path.display());
            return Ok(db_path);
        }
        itunesdb::create_fresh_itunesdb(&db_path)?;
        Ok(db_path)
    }

    /// Run the full DB search and return a human-readable diagnostic log of
    /// every path checked. Useful for surfacing "why can't I find iTunesDB?"
    /// in the application UI.
    pub fn diagnose_db_location(&self) -> Vec<String> {
        let db_name = match self.kind {
            IpodKind::Shuffle => "iTunesSD",
            IpodKind::Classic => "iTunesDB",
        };
        find_db_file_verbose(&self.root, db_name).1
    }

    /// The directory that contains the iTunes database file(s), auto-detected.
    /// Falls back to the conventional path if the directory cannot be found.
    pub fn itunes_dir(&self) -> PathBuf {
        self.find_itunesdb()
            .or_else(|| self.find_itunessd())
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| self.root.join("iPod_Control").join("iTunes"))
    }

    // --- upload ---

    /// Copy `track` onto the device and update the appropriate database.
    ///
    /// Always returns `Ok` if the file was copied. Check `UploadResult::db_updated`
    /// to know whether the device database was also updated.
    pub fn upload(&self, track: &IpodTrack) -> Result<UploadResult> {
        match self.kind {
            IpodKind::Classic => self.upload_classic(track),
            IpodKind::Shuffle => self.upload_shuffle(track),
        }
    }

    fn upload_classic(&self, track: &IpodTrack) -> Result<UploadResult> {
        let mut log: Vec<String> = Vec::new();

        // ── Step 1: copy audio file ───────────────────────────────────────────
        let ext = extension(&track.local_path);
        let dest_path = next_classic_path(&self.music_dir(), &ext)?;

        log.push(format!("→ Copying file to device..."));
        log.push(format!("  src:  {}", track.local_path.display()));
        log.push(format!("  dest: {}", dest_path.display()));

        let bytes_copied = fs::copy(&track.local_path, &dest_path)?;
        log.push(format!("  ✓ Copied {} bytes", bytes_copied));
        info!("Classic copy: {} -> {}", track.local_path.display(), dest_path.display());

        let folder = dest_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("F00");
        let filename = dest_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("AAAA.mp3");
        let ipod_rel = format!("/iPod_Control/Music/{folder}/{filename}");
        let ipod_db_path = path_to_ipod_db(&ipod_rel);
        log.push(format!("  iPod-relative path (display): {ipod_rel}"));
        log.push(format!("  iTunesDB path (colon format):  {ipod_db_path}"));

        // ── Step 2: locate or create iTunesDB ────────────────────────────────
        log.push(format!("→ Locating iTunesDB..."));
        let (existing_db, db_search) = find_db_file_verbose(&self.root, "iTunesDB");
        log.extend(db_search);

        let found_db = match existing_db {
            Some(p) => {
                log.push(format!("  ✓ Using existing DB at {}", p.display()));
                Some(p)
            }
            None => {
                log.push("  DB not found — attempting to create fresh iTunesDB...".into());
                match self.init_database() {
                    Ok(p) => {
                        log.push(format!("  ✓ Created fresh iTunesDB at {}", p.display()));
                        Some(p)
                    }
                    Err(e) => {
                        log.push(format!("  ✗ Could not create iTunesDB: {e}"));
                        None
                    }
                }
            }
        };

        // ── Step 3: update DB ─────────────────────────────────────────────────
        let db_updated = match found_db {
            Some(db_path) => {
                log.push(format!("→ Updating iTunesDB at {}...", db_path.display()));
                let db_track = itunesdb::DbTrack {
                    ipod_rel_path: ipod_db_path,
                    title: track.title.clone(),
                    artist: track.artist.clone(),
                    album: track.album.clone(),
                    duration_ms: track.duration_ms,
                    file_size: track.file_size as u32,
                    bitrate_kbps: track.bitrate_kbps,
                    sample_rate_hz: track.sample_rate_hz,
                    year: track.year,
                };
                match itunesdb::append_track(&db_path, &db_track) {
                    Ok(db_log) => {
                        log.extend(db_log);
                        log.push("  ✓ iTunesDB updated — track will appear under Songs.".into());
                        info!("iTunesDB updated — track will appear under Songs.");
                        true
                    }
                    Err(e) => {
                        log.push(format!("  ✗ iTunesDB write failed: {e}"));
                        warn!("iTunesDB write failed: {e}");
                        false
                    }
                }
            }
            None => {
                log.push("  ✗ No DB available — file copied but will not appear on device.".into());
                false
            }
        };

        Ok(UploadResult { ipod_rel_path: ipod_rel, db_updated, log })
    }

    fn upload_shuffle(&self, track: &IpodTrack) -> Result<UploadResult> {
        let mut log: Vec<String> = Vec::new();

        let ext = extension(&track.local_path);
        let music_dir = self.music_dir();
        fs::create_dir_all(&music_dir)?;

        let existing = fs::read_dir(&music_dir).map(|r| r.count()).unwrap_or(0);
        let filename = index_to_name(existing as u32, &ext);
        let dest_path = music_dir.join(&filename);

        log.push(format!("→ Copying to Shuffle: {}", dest_path.display()));
        let bytes = fs::copy(&track.local_path, &dest_path)?;
        log.push(format!("  ✓ Copied {} bytes", bytes));
        info!("Shuffle copy: {} -> {}", track.local_path.display(), dest_path.display());

        let ipod_rel = format!("/iPod_Control/Music/{filename}");
        log.push(format!("  path: {ipod_rel}"));

        log.push("→ Locating iTunesSD...".into());
        let (found_sd, sd_search) = find_db_file_verbose(&self.root, "iTunesSD");
        log.extend(sd_search);

        let db_updated = match found_sd {
            Some(sd_path) => match itunessd::append_track(&sd_path, &ipod_rel, &ext, track) {
                Ok(_) => {
                    log.push("  ✓ iTunesSD updated.".into());
                    info!("iTunesSD updated.");
                    true
                }
                Err(e) => {
                    log.push(format!("  ✗ iTunesSD update failed: {e}"));
                    warn!("iTunesSD update failed (file was copied): {e}");
                    false
                }
            },
            None => {
                log.push("  ✗ iTunesSD not found — file copied but won't appear on device.".into());
                false
            }
        };

        Ok(UploadResult { ipod_rel_path: ipod_rel, db_updated, log })
    }

    // --- health scan & repair ---

    /// Scan the device for database inconsistencies.
    ///
    /// Only meaningful for Classic/Nano/Mini (iTunesDB) devices.
    /// Returns `Ok(DeviceScanResult::default())` for Shuffle.
    pub fn scan_health(&self) -> Result<DeviceScanResult> {
        if self.kind != IpodKind::Classic {
            return Ok(DeviceScanResult::default());
        }

        let db_path = match self.find_itunesdb() {
            Some(p) => p,
            None => return Err(IpodError::Database("iTunesDB not found on device".into())),
        };

        let db_entries = itunesdb::read_tracks(&db_path)?;

        // Build a set of paths known to the database
        let db_paths: std::collections::HashSet<String> = db_entries
            .iter()
            .map(|e| normalise_ipod_path(&e.ipod_rel_path))
            .collect();

        // Scan all audio files under iPod_Control/Music/
        let mut orphaned_files = Vec::new();
        scan_music_dir(&self.music_dir(), &self.root, &db_paths, &mut orphaned_files);

        // Find mhit records absent from the master playlist.
        // Read all playlist correlation IDs in one pass rather than re-reading
        // the database once per track (which would be O(n²)).
        let playlist_ids = match itunesdb::read_master_playlist_ids(&db_path) {
            Ok(ids) => ids,
            Err(e) => {
                warn!("Could not read master playlist IDs: {e}");
                std::collections::HashSet::new()
            }
        };

        let mut incomplete_entries = Vec::new();
        for entry in &db_entries {
            if !playlist_ids.contains(&entry.track_id) {
                incomplete_entries.push(IncompleteEntry {
                    track_id: entry.track_id,
                    ipod_rel_path: entry.ipod_rel_path.clone(),
                    title: entry.title.clone(),
                });
            }
        }

        Ok(DeviceScanResult {
            orphaned_files,
            incomplete_entries,
        })
    }

    /// List all tracks on the device.
    ///
    /// For Classic/Nano/Mini: reads from iTunesDB if it can be found, otherwise
    /// falls back to a recursive filesystem scan of `iPod_Control/Music/`.
    /// For Shuffle: filesystem scan only (iTunesSD has no track metadata).
    pub fn list_tracks(&self) -> Vec<DeviceTrackEntry> {
        if self.kind == IpodKind::Classic {
            if let Some(db_path) = self.find_itunesdb() {
                match itunesdb::read_tracks(&db_path) {
                    Ok(entries) if !entries.is_empty() => {
                        return entries
                            .into_iter()
                            .map(|e| DeviceTrackEntry {
                                title: if e.title.is_empty() {
                                    stem_from_path(&e.ipod_rel_path)
                                } else {
                                    e.title
                                },
                                artist: e.artist,
                                album: e.album,
                                ipod_rel_path: e.ipod_rel_path,
                                duration_ms: e.duration_ms,
                                from_db: true,
                            })
                            .collect();
                    }
                    Err(e) => warn!("Failed to read iTunesDB for listing: {e}"),
                    _ => {}
                }
            }
        }
        // Fallback: scan filesystem
        let mut out = Vec::new();
        scan_for_device_tracks(&self.music_dir(), &self.root, &mut out);
        out
    }

    /// Repair an incomplete DB entry by adding its `mhip` to the master playlist.
    pub fn repair_incomplete(&self, entry: &IncompleteEntry) -> Result<()> {
        let db_path = self
            .find_itunesdb()
            .ok_or_else(|| IpodError::Database("iTunesDB not found on device".into()))?;
        itunesdb::repair_add_to_master_playlist(&db_path, entry.track_id)
    }

    /// Register an orphaned file into the database (adds both `mhit` and `mhip`).
    ///
    /// Metadata is inferred from the filename only; title defaults to the stem.
    pub fn repair_orphan(&self, orphan: &OrphanedFile) -> Result<()> {
        let db_path = self
            .find_itunesdb()
            .ok_or_else(|| IpodError::Database("iTunesDB not found on device".into()))?;
        let ext = orphan
            .abs_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp3")
            .to_lowercase();
        let title = orphan
            .abs_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();
        let file_size = orphan
            .abs_path
            .metadata()
            .map(|m| m.len() as u32)
            .unwrap_or(0);

        let db_track = itunesdb::DbTrack {
            ipod_rel_path: path_to_ipod_db(&orphan.ipod_rel_path),
            title,
            artist: String::new(),
            album: String::new(),
            duration_ms: 0,
            file_size,
            bitrate_kbps: 0,
            sample_rate_hz: 0,
            year: 0,
        };
        let _ = ext; // used for context, not needed for itunesdb
        itunesdb::append_track(&db_path, &db_track).map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

const NUM_CLASSIC_FOLDERS: u32 = 20;

/// Search `root` for a database file with the given name (e.g. `"iTunesDB"`).
///
/// Strategy (in priority order):
/// 1. Well-known conventional paths.
/// 2. Case-insensitive recursive walk of `iPod_Control/` (max depth 4).
/// 3. Case-insensitive recursive walk of the entire device root (max depth 6).
///
/// The case-insensitive walk is important on Linux/macOS mounts of FAT32
/// volumes where the actual on-disk filename case may differ from what we
/// expect.
fn find_db_file(root: &Path, filename: &str) -> Option<PathBuf> {
    find_db_file_verbose(root, filename).0
}

/// Like `find_db_file` but also returns a human-readable diagnostic log of
/// every location checked, for surfacing to the user when the file is absent.
fn find_db_file_verbose(root: &Path, filename: &str) -> (Option<PathBuf>, Vec<String>) {
    let lower = filename.to_lowercase();
    let mut steps: Vec<String> = Vec::new();

    steps.push(format!("Searching for {} on {}", filename, root.display()));

    // 1. Try known conventional locations in priority order.
    let candidates = [
        root.join("iPod_Control").join("iTunes").join(filename),
        root.join("iPod_Control").join("iTunes_Control").join(filename),
        root.join("iPod_Control").join(filename),
        root.join(filename),
    ];
    for c in &candidates {
        if c.exists() {
            info!("Found {} at {}", filename, c.display());
            steps.push(format!("  ✓ Found at {}", c.display()));
            return (Some(c.clone()), steps);
        } else {
            steps.push(format!("  ✗ {}", c.display()));
        }
    }

    // 2. Case-insensitive walk under iPod_Control/ first (faster).
    let ipod_ctrl = root.join("iPod_Control");
    steps.push(format!("  Scanning {} (case-insensitive, depth 4)…", ipod_ctrl.display()));
    if let Some(p) = walk_find(&ipod_ctrl, &lower, 4) {
        info!("Found {} (search) at {}", filename, p.display());
        steps.push(format!("  ✓ Found at {}", p.display()));
        return (Some(p), steps);
    } else {
        steps.push("    → not found".into());
    }

    // 3. Full device scan as last resort.
    steps.push(format!("  Full device scan {} (depth 6)…", root.display()));
    if let Some(p) = walk_find(root, &lower, 6) {
        info!("Found {} (full scan) at {}", filename, p.display());
        steps.push(format!("  ✓ Found at {}", p.display()));
        return (Some(p), steps);
    } else {
        steps.push("    → not found".into());
    }

    steps.push(format!(
        "{filename} not found. The device may need to be synced \
         with iTunes once to initialise the database."
    ));
    warn!("{} not found on device at {}", filename, root.display());
    (None, steps)
}

/// Recursive case-insensitive filename search.  Returns the first match.
fn walk_find(dir: &Path, lower_name: &str, depth: u8) -> Option<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else { return None };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if path.is_file() && name == lower_name {
            return Some(path);
        }
        if depth > 0 && path.is_dir() {
            if let Some(found) = walk_find(&path, lower_name, depth - 1) {
                return Some(found);
            }
        }
    }
    None
}

fn next_classic_path(music_dir: &Path, ext: &str) -> Result<PathBuf> {
    let mut counts: Vec<(u32, usize)> = (0..NUM_CLASSIC_FOLDERS)
        .map(|n| {
            let dir = music_dir.join(format!("F{n:02}"));
            let _ = fs::create_dir_all(&dir);
            let count = fs::read_dir(&dir).map(|r| r.count()).unwrap_or(0);
            (n, count)
        })
        .collect();

    counts.sort_by_key(|&(_, c)| c);
    let folder_num = counts[0].0;
    let dir = music_dir.join(format!("F{folder_num:02}"));
    let existing = fs::read_dir(&dir).map(|r| r.count()).unwrap_or(0);

    Ok(dir.join(index_to_name(existing as u32, ext)))
}

/// Convert an index to a 4-char uppercase base-26 filename.
pub fn index_to_name(mut idx: u32, ext: &str) -> String {
    let mut chars = [b'A'; 4];
    for i in (0..4).rev() {
        chars[i] = b'A' + (idx % 26) as u8;
        idx /= 26;
    }
    format!(
        "{}.{ext}",
        String::from_utf8(chars.to_vec()).unwrap_or_else(|_| "AAAA".into())
    )
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp3")
        .to_lowercase()
}

/// Normalise an iPod-relative path to lowercase forward-slash form for comparison.
///
/// Handles both `/`-separated paths (internal representation) and
/// `:`-separated paths (iTunesDB on-disk format) so comparisons work regardless
/// of which format a path was stored in.
fn normalise_ipod_path(p: &str) -> String {
    p.replace('\\', "/").replace(':', "/").to_lowercase()
}

/// Convert an iPod-relative forward-slash path to the colon-separator format
/// required by iTunesDB mhod type=2 on Classic/Nano/Mini iPods.
///
/// Example: `/iPod_Control/Music/F00/AAAA.mp3` → `:iPod_Control:Music:F00:AAAA.mp3`
///
/// Reference: <http://www.ipodlinux.org/ITunesDB> — "The path separator is ':'".
fn path_to_ipod_db(rel: &str) -> String {
    rel.replace('/', ":")
}

/// Recursively scan `iPod_Control/Music/` for audio files not in `db_paths`.
fn scan_music_dir(
    dir: &Path,
    root: &Path,
    db_paths: &std::collections::HashSet<String>,
    out: &mut Vec<OrphanedFile>,
) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_music_dir(&path, root, db_paths, out);
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(
            ext.as_str(),
            "mp3" | "aac" | "m4a" | "m4p" | "m4b" | "wav" | "aiff" | "aif" | "flac" | "ogg"
        ) {
            continue;
        }
        // Build iPod-relative path
        let rel = path
            .strip_prefix(root)
            .ok()
            .map(|r| format!("/{}", r.to_string_lossy().replace('\\', "/")))
            .unwrap_or_default();
        if !db_paths.contains(&normalise_ipod_path(&rel)) {
            out.push(OrphanedFile {
                ipod_rel_path: rel,
                abs_path: path,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Volume scanning — enumerate ACTUALLY mounted volumes, not guessed paths
// ---------------------------------------------------------------------------

/// Return every filesystem root that looks like an iPod.
///
/// Uses OS-level mount enumeration rather than guessing paths:
/// - Windows: `GetLogicalDrives()` bitmask → only letters that are mounted
/// - Linux:   `/proc/mounts` → every currently-mounted filesystem
/// - macOS:   `/Volumes/` → standard mount directory for all volumes
///
/// Volumes that are inaccessible (e.g. no read permission) are skipped with
/// a warning rather than causing a hard error.
fn find_ipod_volumes() -> Vec<PathBuf> {
    let volumes = list_mounted_volumes();
    info!("Checking {} mounted volume(s) for iPod_Control", volumes.len());

    let mut found = Vec::new();
    for vol in volumes {
        let ctrl = vol.join("iPod_Control");
        match ctrl.try_exists() {
            Ok(true) if ctrl.is_dir() => {
                info!("iPod found at {}", vol.display());
                found.push(vol);
            }
            Ok(_) => {} // not an iPod volume — skip silently
            Err(e) => {
                // Permission denied or I/O error — note it and move on
                warn!("Cannot check {} — {e}", vol.display());
            }
        }
    }
    found
}

/// Return the list of filesystem roots that are currently mounted.
fn list_mounted_volumes() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        windows_mounted_volumes()
    }
    #[cfg(target_os = "linux")]
    {
        proc_mounts_volumes()
    }
    #[cfg(target_os = "macos")]
    {
        // macOS mounts all removable media under /Volumes — enumerate its children
        // rather than hard-coding a path.
        let mut vols: Vec<PathBuf> = Vec::new();
        vols.push(PathBuf::from("/")); // root filesystem
        if let Ok(entries) = fs::read_dir("/Volumes") {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    vols.push(p);
                }
            }
        }
        vols
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Windows: call `GetLogicalDrives()` to get a bitmask of mounted drive letters.
///
/// Bit 0 = A:, bit 1 = B:, bit 2 = C:, …, bit 25 = Z:.
/// Only bits set in the bitmask correspond to drives that Windows has mounted —
/// no need to probe 24 paths and swallow access errors from dead letters.
#[cfg(target_os = "windows")]
fn windows_mounted_volumes() -> Vec<PathBuf> {
    extern "system" {
        fn GetLogicalDrives() -> u32;
    }
    let bitmask = unsafe { GetLogicalDrives() };
    (0u32..26)
        .filter(|&bit| bitmask & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", (b'A' + bit as u8) as char)))
        .inspect(|p| info!("Mounted volume: {}", p.display()))
        .collect()
}

/// Linux: parse `/proc/mounts` for every currently-mounted filesystem.
///
/// Each line: `device mountpoint fstype options dump pass`
/// Mount-point paths may contain octal escape sequences (e.g. `\040` → space).
#[cfg(target_os = "linux")]
fn proc_mounts_volumes() -> Vec<PathBuf> {
    let content = match fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(e) => {
            warn!("Cannot read /proc/mounts: {e}");
            return Vec::new();
        }
    };
    let mut seen = std::collections::HashSet::new();
    content
        .lines()
        .filter_map(|line| {
            let mp = line.split_ascii_whitespace().nth(1)?;
            let path = PathBuf::from(unescape_mountpoint(mp));
            if seen.insert(path.clone()) {
                info!("Mounted volume: {}", path.display());
                Some(path)
            } else {
                None
            }
        })
        .collect()
}

/// Unescape octal sequences in Linux mount-point paths (e.g. `\040` → ` `).
#[cfg(target_os = "linux")]
fn unescape_mountpoint(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(n as char);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[allow(dead_code)]
fn scan_dir_for_ipods(base: &Path, out: &mut Vec<PathBuf>, depth: u8) {
    let Ok(entries) = fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("iPod_Control").is_dir() {
            out.push(path);
        } else if depth > 0 && path.is_dir() {
            scan_dir_for_ipods(&path, out, depth - 1);
        }
    }
}

fn build_device(root: PathBuf) -> Result<IpodDevice> {
    let itunes = root.join("iPod_Control").join("iTunes");
    let kind = if itunes.join("iTunesSD").exists() {
        IpodKind::Shuffle
    } else {
        IpodKind::Classic
    };

    let label = read_sys_info_name(&root).unwrap_or_else(|| {
        root.to_string_lossy()
            .trim_end_matches(|c| c == '/' || c == '\\')
            .split(|c| c == '/' || c == '\\')
            .last()
            .unwrap_or("iPod")
            .to_string()
    });

    let firmware = probe_firmware(&root, &kind, &itunes);

    Ok(IpodDevice {
        root,
        kind,
        label,
        firmware,
    })
}

// ---------------------------------------------------------------------------
// Firmware / hardware probing
// ---------------------------------------------------------------------------

/// Read hardware info from SysInfo and iTunesDB to build a `FirmwareInfo`.
fn probe_firmware(root: &Path, kind: &IpodKind, _itunes: &Path) -> FirmwareInfo {
    let mut info = FirmwareInfo::default();

    // Read SysInfo for model and hardware version
    let sysinfo_path = root.join("iPod_Control").join("Device").join("SysInfo");
    if let Ok(content) = fs::read_to_string(&sysinfo_path) {
        if let Some(v) = sysinfo_key(&content, "ModelNumStr") {
            info.model_str = v;
        }
        if let Some(v) = sysinfo_key(&content, "HardwareVersion") {
            info.hw_version_str = v;
        }
        // SysInfoExtended (XML, newer models) takes precedence for model
        let extended_path = root
            .join("iPod_Control")
            .join("Device")
            .join("SysInfoExtended");
        if let Ok(xml) = fs::read_to_string(&extended_path) {
            if let Some(model) = xml_value(&xml, "ModelNumStr") {
                info.model_str = model;
            }
        }
    }

    // Read DB version from iTunesDB header
    if *kind == IpodKind::Classic {
        if let Some(db_path) = find_db_file(root, "iTunesDB") {
            if let Ok(data) = fs::read(&db_path) {
                if data.len() >= 20 && &data[0..4] == b"mhbd" {
                    let version = u32::from_le_bytes(data[16..20].try_into().unwrap_or([0; 4]));
                    info.db_version = version;
                    info!(
                        "Detected iTunesDB version {} for model '{}'",
                        version, info.model_str
                    );
                }
            }
        }
    }

    info.generation = derive_generation(&info.model_str, info.db_version, kind);
    info
}

/// Derive a human-readable generation string from model and DB version.
fn derive_generation(model_str: &str, db_version: u32, kind: &IpodKind) -> String {
    // Well-known model identifiers
    // Reference: https://www.theiphonewiki.com/wiki/Models
    let from_model = match model_str {
        // 1st gen (Scroll Wheel)
        s if s.starts_with("M8513") || s.starts_with("M8541") || s.starts_with("M8697")
            || s.starts_with("M8709") =>
        {
            Some("1st generation")
        }
        // 2nd gen (Touch Wheel)
        s if s.starts_with("M8737") || s.starts_with("M8740") || s.starts_with("M8742") =>
        {
            Some("2nd generation")
        }
        // 3rd gen
        s if s.starts_with("M8976") || s.starts_with("M8946") || s.starts_with("M8948") =>
        {
            Some("3rd generation")
        }
        // 4th gen (Click Wheel)
        s if s.starts_with("M9282") || s.starts_with("M9268") || s.starts_with("MA079")
            || s.starts_with("MA080") || s.starts_with("MA446") =>
        {
            Some("4th generation")
        }
        // 4th gen Photo / Color
        s if s.starts_with("M9829") || s.starts_with("M9585") || s.starts_with("M9586")
            || s.starts_with("M9830") =>
        {
            Some("4th generation (Photo)")
        }
        // Mini 1st gen
        s if s.starts_with("M9160") || s.starts_with("M9161") || s.starts_with("M9162")
            || s.starts_with("M9163") || s.starts_with("M9164") =>
        {
            Some("Mini 1st generation")
        }
        // Mini 2nd gen
        s if s.starts_with("M9800") || s.starts_with("M9802") || s.starts_with("M9804")
            || s.starts_with("M9806") =>
        {
            Some("Mini 2nd generation")
        }
        // 5th gen (Video)
        s if s.starts_with("MA002") || s.starts_with("MA004") || s.starts_with("MA146")
            || s.starts_with("MA147") || s.starts_with("MA448") || s.starts_with("MA450") =>
        {
            Some("5th generation")
        }
        // 6th gen Classic
        s if s.starts_with("MA489") || s.starts_with("MA491") || s.starts_with("MB029")
            || s.starts_with("MB147") || s.starts_with("MC293") || s.starts_with("MC297") =>
        {
            Some("Classic (6th generation)")
        }
        // Nano 1st gen
        s if s.starts_with("MA350") || s.starts_with("MA352") || s.starts_with("MA004")
            || s.starts_with("MA005") =>
        {
            Some("Nano 1st generation")
        }
        // Nano 2nd gen
        s if s.starts_with("MA477") || s.starts_with("MA428") || s.starts_with("MA487")
            || s.starts_with("MA489") =>
        {
            Some("Nano 2nd generation")
        }
        // Shuffle 1st gen
        s if s.starts_with("M9724") || s.starts_with("M9725") => Some("Shuffle 1st generation"),
        // Shuffle 2nd gen
        s if s.starts_with("MA564") || s.starts_with("MA947") => Some("Shuffle 2nd generation"),
        // Shuffle 3rd gen
        s if s.starts_with("MC193") || s.starts_with("MC164") => Some("Shuffle 3rd generation"),
        _ => None,
    };

    if let Some(gen) = from_model {
        return gen.to_string();
    }

    // Fallback: infer from DB version
    match kind {
        IpodKind::Shuffle => "Shuffle".to_string(),
        IpodKind::Classic => match db_version {
            0..=4 => "1st–2nd generation".to_string(),
            5..=6 => "3rd generation".to_string(),
            7..=8 => "Mini 1st generation".to_string(),
            9 => "4th generation".to_string(),
            10 => "5th generation / Nano 1st gen".to_string(),
            11 => "Nano 2nd generation".to_string(),
            12..=13 => "Classic / Nano 3rd–5th gen".to_string(),
            _ => format!("Unknown (DB v{db_version})"),
        },
    }
}

/// Extract a plain-text `Key: Value` from SysInfo content.
fn sysinfo_key(content: &str, key: &str) -> Option<String> {
    content
        .lines()
        .find(|l| l.starts_with(key))?
        .split_once(':')?
        .1
        .trim()
        .to_string()
        .into()
}

/// Extract the first occurrence of `<key>value</key>` from SysInfoExtended XML.
fn xml_value(xml: &str, key: &str) -> Option<String> {
    let open = format!("<{key}>");
    let close = format!("</{key}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

fn read_sys_info_name(root: &Path) -> Option<String> {
    let path = root.join("iPod_Control").join("Device").join("SysInfo");
    let content = fs::read_to_string(path).ok()?;
    sysinfo_key(&content, "UserName")
        .filter(|s| !s.is_empty())
        .or_else(|| sysinfo_key(&content, "ModelNumStr"))
}

fn free_space_bytes(_path: &Path) -> Option<u64> {
    // TODO: implement via GetDiskFreeSpaceExW / statvfs
    None
}

/// Recursively scan `iPod_Control/Music/` and collect audio files as
/// `DeviceTrackEntry` records (filesystem fallback when iTunesDB is absent).
fn scan_for_device_tracks(dir: &Path, root: &Path, out: &mut Vec<DeviceTrackEntry>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_for_device_tracks(&path, root, out);
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !matches!(
            ext.as_str(),
            "mp3" | "aac" | "m4a" | "m4p" | "m4b" | "wav" | "aiff" | "aif" | "flac"
        ) {
            continue;
        }
        let ipod_rel = path
            .strip_prefix(root)
            .ok()
            .map(|r| format!("/{}", r.to_string_lossy().replace('\\', "/")))
            .unwrap_or_default();
        // Use full filename (e.g. "AAAA.mp3") so the format is visible in the UI.
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        out.push(DeviceTrackEntry {
            title: filename,
            artist: String::new(),
            album: String::new(),
            ipod_rel_path: ipod_rel,
            duration_ms: 0,
            from_db: false,
        });
    }
}

/// Extract a display-friendly filename stem from an iPod-relative path.
fn stem_from_path(ipod_rel: &str) -> String {
    std::path::Path::new(ipod_rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(ipod_rel)
        .to_string()
}
