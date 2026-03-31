pub mod scanner;

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
}

impl Library {
    pub fn new(tracks: Vec<Track>) -> Self {
        Self {
            all_tracks: tracks.clone(),
            tracks,
            selected_index: 0,
            active_playlist: None,
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
