pub mod cache;
pub mod dedup;
pub mod magic;
pub mod scanner;

// ---------------------------------------------------------------------------
// Sort order / group-by
// ---------------------------------------------------------------------------

/// Preset sort orders for the library list.
///
/// Variants prefixed with `GroupBy` render visual section-separator headers
/// between groups (see [`SortOrder::has_sections`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Restore the original scan order (artist / album / title).
    Original,
    #[allow(dead_code)]
    /// Artist → Album → Title (the default view).
    ArtistAlbumTitle,
    /// Title alphabetical.
    Title,
    /// Album → Track title.
    Album,
    /// Longest first.
    DurationDesc,
    /// Most recently added (file modification time, newest first).
    DateAdded,
    /// Grouped sections by verified file extension (MP3, FLAC, XM, …).
    GroupByExtension,
    /// Grouped sections by artist name.
    GroupByArtist,
    /// Grouped sections by release year.
    GroupByYear,
    /// Grouped sections by month the file was added (mtime year + month).
    GroupByMonth,
    /// Grouped sections by the track's first user-defined tag.
    GroupByTag,
}

impl Default for SortOrder {
    fn default() -> Self { Self::ArtistAlbumTitle }
}

impl SortOrder {
    /// Cycle to the next preset, wrapping back to `Original`.
    pub fn next(self) -> Self {
        match self {
            Self::Original         => Self::ArtistAlbumTitle,
            Self::ArtistAlbumTitle => Self::Title,
            Self::Title            => Self::Album,
            Self::Album            => Self::DurationDesc,
            Self::DurationDesc     => Self::DateAdded,
            Self::DateAdded        => Self::GroupByExtension,
            Self::GroupByExtension => Self::GroupByArtist,
            Self::GroupByArtist    => Self::GroupByYear,
            Self::GroupByYear      => Self::GroupByMonth,
            Self::GroupByMonth     => Self::GroupByTag,
            Self::GroupByTag       => Self::Original,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Original         => "Original Order",
            Self::ArtistAlbumTitle => "Artist / Album",
            Self::Title            => "Title",
            Self::Album            => "Album",
            Self::DurationDesc     => "Duration ↓",
            Self::DateAdded        => "Date Added",
            Self::GroupByExtension => "Group by Extension",
            Self::GroupByArtist    => "Group by Artist",
            Self::GroupByYear      => "Group by Year",
            Self::GroupByMonth     => "Group by Month",
            Self::GroupByTag       => "Group by Tag",
        }
    }

    /// Whether this sort order renders visual section separators between groups.
    pub fn has_sections(self) -> bool {
        matches!(
            self,
            Self::GroupByExtension | Self::GroupByArtist
            | Self::GroupByYear    | Self::GroupByMonth
            | Self::GroupByTag
        )
    }

    /// Return the section-header key for `track` under this sort order.
    /// Returns `None` for sort orders that don't use section headers.
    pub fn section_key(self, track: &Track) -> Option<String> {
        match self {
            Self::GroupByExtension => Some(
                track.path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_uppercase())
                    .unwrap_or_else(|| "OTHER".into()),
            ),
            Self::GroupByArtist => Some(
                if track.artist.is_empty() {
                    "Unknown Artist".into()
                } else {
                    track.artist.clone()
                },
            ),
            Self::GroupByYear => Some(
                track.year
                    .map(|y| y.to_string())
                    .unwrap_or_else(|| "Unknown Year".into()),
            ),
            Self::GroupByMonth => {
                let secs = std::fs::metadata(&track.path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                Some(match secs {
                    None => "Unknown Date".into(),
                    Some(s) => {
                        let (y, mo) = unix_year_month(s);
                        const MONTHS: [&str; 12] = [
                            "January", "February", "March",     "April",   "May",      "June",
                            "July",    "August",   "September", "October", "November", "December",
                        ];
                        let name = MONTHS.get(mo.saturating_sub(1) as usize).copied().unwrap_or("?");
                        format!("{y} · {name}")
                    }
                })
            }
            Self::GroupByTag => None, // handled by Library::section_key which has tag_sort_keys access
            _ => None,
        }
    }
}

/// Convert a UNIX timestamp (seconds) to `(year, month)` using the
/// proleptic Gregorian calendar.  Month is 1-based (1 = January).
fn unix_year_month(secs: u64) -> (i32, u32) {
    // Howard Hinnant's civil_from_days algorithm
    let days = (secs / 86_400) as i64;
    let z    = days + 719_468;
    let era  = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe  = (z - era * 146_097) as u32;
    let yoe  = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy  = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp   = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year  = yoe as i64 + era * 400 + if month <= 2 { 1 } else { 0 };
    (year as i32, month)
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::media::{MediaCapability, MediaFormat, MediaItem};

/// A single audio track with full metadata.
#[derive(Debug, Clone)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<u32>,
    pub duration_secs: Option<u32>,
    pub file_size: u64,
    pub bitrate_kbps: Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u8>,
}

impl MediaItem for Track {
    fn path(&self) -> &Path { &self.path }
    fn title(&self) -> &str { &self.title }
    fn artist(&self) -> &str { &self.artist }
    fn album(&self) -> &str { &self.album }
    fn year(&self) -> Option<u32> { self.year }
    fn duration_secs(&self) -> Option<u32> { self.duration_secs }
    fn file_size(&self) -> u64 { self.file_size }

    fn format(&self) -> MediaFormat {
        let ext = self.path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        if crate::tracker::is_tracker_ext(&ext) {
            MediaFormat::Tracker
        } else {
            MediaFormat::Standard
        }
    }

    fn capabilities(&self) -> &'static [MediaCapability] {
        match self.format() {
            MediaFormat::Standard => &[
                MediaCapability::TagEdit,
                MediaCapability::IpodTransfer,
                MediaCapability::StandardPlayback,
            ],
            MediaFormat::Tracker => &[
                MediaCapability::TrackerPlayback,
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// TrackEdit — caller-supplied metadata changes
// ---------------------------------------------------------------------------

/// Metadata fields that can be written back to an audio file's tags.
///
/// Every field is `Option` — only `Some` values are written; `None` leaves
/// the existing tag value untouched.
///
/// # Example
/// ```no_run
/// use console_music_player::library::TrackEdit;
/// let edit = TrackEdit { title: Some("New Title".into()), ..TrackEdit::default() };
/// ```
#[derive(Debug, Clone, Default)]
pub struct TrackEdit {
    pub title:        Option<String>,
    pub artist:       Option<String>,
    pub album:        Option<String>,
    /// Release year (e.g. `2006`).
    pub year:         Option<u32>,
    /// 1-based track number within the album.
    pub track_number: Option<u32>,
    pub genre:        Option<String>,
}

// ---------------------------------------------------------------------------
// Library
// ---------------------------------------------------------------------------

/// In-memory music library, optionally filtered by a loaded playlist.
#[derive(Debug, Default)]
pub struct Library {
    /// The full scanned set — never filtered.
    pub all_tracks: Vec<Track>,
    /// Currently displayed set (may be a playlist subset).
    pub tracks: Vec<Track>,
    pub selected_index: usize,
    /// Name of the playlist currently loaded, if any.
    pub active_playlist: Option<String>,
    /// Current sort preset applied to `tracks`.
    pub sort_order: SortOrder,
    /// Paths in the original scan order — used to restore `SortOrder::Original`.
    original_paths: Vec<PathBuf>,
    /// Primary tag per path, precomputed for GroupByTag sorting/sections.
    tag_sort_keys: HashMap<PathBuf, String>,
}

impl Library {
    pub fn new(tracks: Vec<Track>) -> Self {
        let original_paths = tracks.iter().map(|t| t.path.clone()).collect();
        Self {
            all_tracks: tracks.clone(),
            tracks,
            selected_index: 0,
            active_playlist: None,
            sort_order: SortOrder::default(),
            original_paths,
            tag_sort_keys: HashMap::new(),
        }
    }

    /// Filter the displayed tracks to those whose paths are in `paths`.
    pub fn load_playlist(&mut self, name: &str, paths: &[PathBuf]) {
        self.tracks = self
            .all_tracks
            .iter()
            .filter(|t| paths.contains(&t.path))
            .cloned()
            .collect();
        self.selected_index = 0;
        self.active_playlist = Some(name.to_string());
    }

    /// Restore the full unfiltered view.
    pub fn clear_playlist(&mut self) {
        self.tracks = self.all_tracks.clone();
        self.selected_index = 0;
        self.active_playlist = None;
        self.apply_sort();
    }

    /// Advance to the next sort preset and re-sort the displayed list.
    pub fn cycle_sort(&mut self) {
        self.sort_order = self.sort_order.next();
        self.apply_sort();
        self.selected_index = 0;
    }

    /// Re-sort `tracks` according to the current `sort_order`.
    pub fn apply_sort(&mut self) {
        match self.sort_order {
            SortOrder::Original => {
                // Restore the order tracks had when they were first scanned.
                let pos: std::collections::HashMap<&PathBuf, usize> =
                    self.original_paths.iter().enumerate().map(|(i, p)| (p, i)).collect();
                self.tracks.sort_by_key(|t| pos.get(&t.path).copied().unwrap_or(usize::MAX));
            }
            SortOrder::ArtistAlbumTitle => {
                self.tracks.sort_by(|a, b| {
                    a.artist.cmp(&b.artist)
                        .then_with(|| a.album.cmp(&b.album))
                        .then_with(|| a.title.cmp(&b.title))
                });
            }
            SortOrder::Title => {
                self.tracks.sort_by(|a, b| a.title.cmp(&b.title));
            }
            SortOrder::Album => {
                self.tracks.sort_by(|a, b| {
                    a.album.cmp(&b.album).then_with(|| a.title.cmp(&b.title))
                });
            }
            SortOrder::DurationDesc => {
                self.tracks.sort_by(|a, b| b.duration_secs.cmp(&a.duration_secs));
            }
            SortOrder::DateAdded => {
                // Best available proxy on all platforms: file modification time.
                // Newer files sort first.
                self.tracks.sort_by(|a, b| {
                    let mt = |p: &PathBuf| {
                        std::fs::metadata(p).and_then(|m| m.modified()).ok()
                    };
                    mt(&b.path).cmp(&mt(&a.path))
                });
            }
            SortOrder::GroupByExtension => {
                self.tracks.sort_by(|a, b| {
                    let ext = |t: &Track| {
                        t.path.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_lowercase())
                            .unwrap_or_default()
                    };
                    ext(a).cmp(&ext(b))
                        .then_with(|| a.artist.cmp(&b.artist))
                        .then_with(|| a.album.cmp(&b.album))
                        .then_with(|| a.title.cmp(&b.title))
                });
            }
            SortOrder::GroupByArtist => {
                self.tracks.sort_by(|a, b| {
                    a.artist.cmp(&b.artist)
                        .then_with(|| a.album.cmp(&b.album))
                        .then_with(|| a.title.cmp(&b.title))
                });
            }
            SortOrder::GroupByYear => {
                self.tracks.sort_by(|a, b| {
                    // None (unknown year) sorts last.
                    match (a.year, b.year) {
                        (None,    None)    => std::cmp::Ordering::Equal,
                        (None,    Some(_)) => std::cmp::Ordering::Greater,
                        (Some(_), None)    => std::cmp::Ordering::Less,
                        (Some(ya), Some(yb)) => ya.cmp(&yb),
                    }
                    .then_with(|| a.artist.cmp(&b.artist))
                    .then_with(|| a.title.cmp(&b.title))
                });
            }
            SortOrder::GroupByMonth => {
                self.tracks.sort_by(|a, b| {
                    let month_key = |t: &Track| {
                        std::fs::metadata(&t.path)
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|st| st.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| unix_year_month(d.as_secs()))
                    };
                    // None (unreadable mtime) sorts last.
                    match (month_key(a), month_key(b)) {
                        (None, None)       => std::cmp::Ordering::Equal,
                        (None, Some(_))    => std::cmp::Ordering::Greater,
                        (Some(_), None)    => std::cmp::Ordering::Less,
                        (Some(ka), Some(kb)) => ka.cmp(&kb),
                    }
                    .then_with(|| a.artist.cmp(&b.artist))
                    .then_with(|| a.title.cmp(&b.title))
                });
            }
            SortOrder::GroupByTag => {
                // Clone to avoid simultaneous mutable/immutable borrow of self.
                let tag_keys = self.tag_sort_keys.clone();
                self.tracks.sort_by(|a, b| {
                    // Untagged entries sort after all tagged groups.
                    let ka = tag_keys.get(&a.path).map(String::as_str).unwrap_or("\u{FFFF}");
                    let kb = tag_keys.get(&b.path).map(String::as_str).unwrap_or("\u{FFFF}");
                    ka.cmp(kb)
                        .then_with(|| a.artist.cmp(&b.artist))
                        .then_with(|| a.title.cmp(&b.title))
                });
            }
        }
    }

    /// Update the tag sort keys (called whenever the tag store changes).
    pub fn set_tag_sort_keys(&mut self, keys: HashMap<PathBuf, String>) {
        self.tag_sort_keys = keys;
    }

    /// Return the section header key for `track` under the current sort order.
    /// Handles `GroupByTag` (which needs `tag_sort_keys`) in addition to all
    /// `SortOrder::section_key` variants.
    pub fn section_key(&self, track: &Track) -> Option<String> {
        if self.sort_order == SortOrder::GroupByTag {
            Some(
                self.tag_sort_keys
                    .get(&track.path)
                    .cloned()
                    .unwrap_or_else(|| "Untagged".into()),
            )
        } else {
            self.sort_order.section_key(track)
        }
    }

    pub fn selected(&self) -> Option<&Track> {
        self.tracks.get(self.selected_index)
    }

    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected_index + 1 < self.tracks.len() {
            self.selected_index += 1;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected_index = self.selected_index.saturating_sub(page);
    }

    pub fn page_down(&mut self, page: usize) {
        let max = self.tracks.len().saturating_sub(1);
        self.selected_index = (self.selected_index + page).min(max);
    }

    /// Write `edit` to the audio file at `path` and update the matching
    /// in-memory `Track`.
    ///
    /// Only the fields present in the `Track` struct (title, artist, album)
    /// are updated in memory; the remaining fields (year, track_number, genre)
    /// are written to the file but are not currently stored in `Track`.
    ///
    /// Returns an error if the file cannot be opened or the tag cannot be
    /// saved.  On error the in-memory state is not changed.
    pub fn apply_edit(&mut self, path: &Path, edit: &TrackEdit) -> crate::error::Result<()> {
        scanner::write_metadata(path, edit)?;

        // Update both the full library and the active (possibly playlist-filtered) view.
        for collection in [&mut self.all_tracks, &mut self.tracks] {
            for track in collection.iter_mut() {
                if track.path == path {
                    if let Some(ref t) = edit.title  { track.title  = t.clone(); }
                    if let Some(ref a) = edit.artist { track.artist = a.clone(); }
                    if let Some(ref a) = edit.album  { track.album  = a.clone(); }
                    if let Some(y)     = edit.year   { track.year   = Some(y); }
                }
            }
        }

        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}
