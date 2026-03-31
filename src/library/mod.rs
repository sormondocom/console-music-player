pub mod dedup;
pub mod scanner;

// ---------------------------------------------------------------------------
// Sort order
// ---------------------------------------------------------------------------

/// Preset sort orders for the library list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    /// Artist → Album → Title  (the default scan order)
    ArtistAlbumTitle,
    /// Title alphabetical
    Title,
    /// Album → Track title
    Album,
    /// Longest first
    DurationDesc,
    /// Most recently added (highest inode / OS-assigned order, approximation)
    DateAdded,
}

impl SortOrder {
    /// Cycle to the next preset.
    pub fn next(self) -> Self {
        match self {
            Self::ArtistAlbumTitle => Self::Title,
            Self::Title            => Self::Album,
            Self::Album            => Self::DurationDesc,
            Self::DurationDesc     => Self::DateAdded,
            Self::DateAdded        => Self::ArtistAlbumTitle,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ArtistAlbumTitle => "Artist / Album",
            Self::Title            => "Title",
            Self::Album            => "Album",
            Self::DurationDesc     => "Duration ↓",
            Self::DateAdded        => "Date Added",
        }
    }
}

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
}

impl Library {
    pub fn new(tracks: Vec<Track>) -> Self {
        Self {
            all_tracks: tracks.clone(),
            tracks,
            selected_index: 0,
            active_playlist: None,
            sort_order: SortOrder::default(),
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
                self.tracks.sort_by(|a, b| {
                    b.duration_secs.cmp(&a.duration_secs)
                });
            }
            SortOrder::DateAdded => {
                // Best available proxy on all platforms: file modification time.
                // Newer files sort first.
                self.tracks.sort_by(|a, b| {
                    let mt = |p: &std::path::PathBuf| {
                        std::fs::metadata(p)
                            .and_then(|m| m.modified())
                            .ok()
                    };
                    mt(&b.path).cmp(&mt(&a.path))
                });
            }
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
