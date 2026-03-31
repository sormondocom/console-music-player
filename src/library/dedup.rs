//! Duplicate-track detection for the local library.
//!
//! Two kinds of duplicates are detected:
//!
//! * **ExactContent** — files that share the same file size *and* a
//!   64-bit fingerprint of their first + last 64 KB.  These are almost
//!   certainly byte-identical copies.
//!
//! * **MetadataMatch** — files whose normalised title and artist strings
//!   are identical but whose content may differ (e.g. different encodes
//!   or bit-rates of the same track).
//!
//! The fingerprint is intentionally cheap (no full-file hash) so scanning
//! a large library is fast.  It is strong enough to distinguish virtually
//! all real-world audio files of the same size.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::Path;

use crate::library::Track;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Why a group of tracks is considered duplicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateKind {
    /// Identical file size and content fingerprint — almost certainly the same
    /// bytes on disk, just stored in different locations.
    ExactContent,
    /// Same normalised title + artist but potentially different encodes.
    MetadataMatch,
}

/// One candidate within a duplicate group.
#[derive(Debug, Clone)]
pub struct DuplicateCandidate {
    pub track: Track,
    /// Content fingerprint (hash of first + last 64 KB).  Present for
    /// `ExactContent` groups; `None` for `MetadataMatch` groups where hashing
    /// is skipped because the files are likely different encodes.
    pub checksum: Option<u64>,
}

/// A set of tracks that appear to be duplicates of one another.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub kind: DuplicateKind,
    pub candidates: Vec<DuplicateCandidate>,
}

impl DuplicateGroup {
    /// Suggested default action for each candidate index.
    ///
    /// For `ExactContent` groups the highest-quality copy is suggested as
    /// Keep (highest bitrate; most metadata fields filled as tiebreaker).
    /// All others are suggested as Delete.
    ///
    /// For `MetadataMatch` groups the same heuristic applies but actions are
    /// left as `Undecided` for all-but-the-best so the user reviews before
    /// deleting content that may genuinely differ.
    pub fn suggested_actions(&self) -> Vec<DedupAction> {
        let best = self.best_candidate_index();
        self.candidates
            .iter()
            .enumerate()
            .map(|(i, _)| {
                if i == best {
                    DedupAction::Keep
                } else if self.kind == DuplicateKind::ExactContent {
                    DedupAction::Delete
                } else {
                    DedupAction::Undecided
                }
            })
            .collect()
    }

    /// Index of the "best" candidate by quality heuristic.
    fn best_candidate_index(&self) -> usize {
        self.candidates
            .iter()
            .enumerate()
            .max_by_key(|(_, c)| {
                let bitrate = c.track.bitrate_kbps.unwrap_or(0) as u32;
                let meta_score = (!c.track.title.is_empty()) as u32
                    + (!c.track.artist.is_empty()) as u32
                    + (!c.track.album.is_empty()) as u32
                    + c.track.year.is_some() as u32;
                (bitrate, meta_score, c.track.file_size)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

/// Per-candidate action chosen by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupAction {
    Undecided,
    Keep,
    Delete,
}

impl DedupAction {
    /// Cycle: Undecided → Keep → Delete → Undecided
    pub fn cycle(self) -> Self {
        match self {
            Self::Undecided => Self::Keep,
            Self::Keep => Self::Delete,
            Self::Delete => Self::Undecided,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Undecided => " ? ",
            Self::Keep      => "KEEP",
            Self::Delete    => " DEL",
        }
    }
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Scan `tracks` and return all duplicate groups (each with ≥ 2 candidates).
///
/// Groups are ordered: `ExactContent` first, then `MetadataMatch`.
pub fn find_duplicates(tracks: &[Track]) -> Vec<DuplicateGroup> {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    let mut exact_indices: HashSet<usize> = HashSet::new();

    // ── Phase 1: exact-content duplicates ────────────────────────────────────
    // Pre-filter by file size (cheap), then fingerprint only the collisions.
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, track) in tracks.iter().enumerate() {
        if track.file_size > 0 {
            by_size.entry(track.file_size).or_default().push(i);
        }
    }

    for (_size, indices) in &by_size {
        if indices.len() < 2 {
            continue;
        }

        // Fingerprint each candidate in this size bucket.
        let fingerprints: Vec<(usize, Option<u64>)> = indices
            .iter()
            .map(|&i| (i, file_fingerprint(&tracks[i].path)))
            .collect();

        // Group by fingerprint value.
        let mut by_fp: HashMap<u64, Vec<usize>> = HashMap::new();
        for &(i, fp) in &fingerprints {
            if let Some(fp) = fp {
                by_fp.entry(fp).or_default().push(i);
            }
        }

        let fp_map: HashMap<usize, u64> =
            fingerprints.iter().filter_map(|&(i, fp)| fp.map(|v| (i, v))).collect();

        for (_fp, group_idx) in by_fp {
            if group_idx.len() < 2 {
                continue;
            }
            let candidates = group_idx
                .iter()
                .map(|&i| DuplicateCandidate {
                    track: tracks[i].clone(),
                    checksum: fp_map.get(&i).copied(),
                })
                .collect();
            for &i in &group_idx {
                exact_indices.insert(i);
            }
            groups.push(DuplicateGroup { kind: DuplicateKind::ExactContent, candidates });
        }
    }

    // ── Phase 2: metadata-match duplicates ────────────────────────────────────
    // Skip tracks already placed in an exact-content group so they don't
    // appear twice.
    let mut by_meta: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, track) in tracks.iter().enumerate() {
        if exact_indices.contains(&i) {
            continue;
        }
        let norm_title = normalize(track.title.trim());
        let norm_artist = normalize(track.artist.trim());
        // Skip tracks with no usable metadata — they'd generate enormous false-positive groups.
        if norm_title.is_empty() {
            continue;
        }
        by_meta.entry((norm_title, norm_artist)).or_default().push(i);
    }

    for (_key, indices) in by_meta {
        if indices.len() < 2 {
            continue;
        }
        let candidates = indices
            .iter()
            .map(|&i| DuplicateCandidate { track: tracks[i].clone(), checksum: None })
            .collect();
        groups.push(DuplicateGroup { kind: DuplicateKind::MetadataMatch, candidates });
    }

    // Exact-content groups first, then metadata matches.
    groups.sort_by_key(|g| match g.kind {
        DuplicateKind::ExactContent  => 0,
        DuplicateKind::MetadataMatch => 1,
    });

    groups
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute a fast 64-bit fingerprint of a file by hashing:
///  * the file size
///  * the first 64 KB
///  * the last 64 KB (if the file is long enough)
///
/// Returns `None` if the file cannot be opened or read.
fn file_fingerprint(path: &Path) -> Option<u64> {
    const CHUNK: usize = 65_536; // 64 KB

    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();

    let mut hasher = DefaultHasher::new();
    size.hash(&mut hasher);

    let mut buf = vec![0u8; CHUNK];

    let n = file.read(&mut buf).ok()?;
    buf[..n].hash(&mut hasher);

    if size > (CHUNK * 2) as u64 {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::End(-(CHUNK as i64))).ok()?;
        let n = file.read(&mut buf).ok()?;
        buf[..n].hash(&mut hasher);
    }

    Some(hasher.finish())
}

/// Lowercase + collapse whitespace + strip non-alphanumeric for fuzzy comparison.
fn normalize(s: &str) -> String {
    let filtered: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .flat_map(|c| c.to_lowercase())
        .collect();
    filtered.split_whitespace().collect::<Vec<_>>().join(" ")
}
