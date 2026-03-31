use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::*;
use lofty::probe::Probe;
use tracing::{debug, warn};
use walkdir::WalkDir;

use super::{Track, TrackEdit};
use crate::error::{AppError, Result};
use crate::tracker;

/// File extensions handled by the lofty tag reader / symphonia decoder.
const LOFTY_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "ogg", "opus", "wav", "aiff", "aif",
];

/// Scan multiple directories, merge results, and deduplicate by path.
///
/// Directories that fail to scan are logged as warnings and skipped —
/// the remaining sources are still returned.
pub fn scan_directories(roots: &[PathBuf]) -> Result<Vec<Track>> {
    let mut tracks: Vec<Track> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for root in roots {
        match scan_directory(root) {
            Ok(found) => {
                for track in found {
                    if seen.insert(track.path.clone()) {
                        tracks.push(track);
                    }
                }
            }
            Err(e) => warn!("Scan failed for {}: {e}", root.display()),
        }
    }

    tracks.sort_by(|a, b| {
        a.artist
            .cmp(&b.artist)
            .then_with(|| a.album.cmp(&b.album))
            .then_with(|| a.title.cmp(&b.title))
    });

    Ok(tracks)
}

/// Recursively scan a single directory and return all discovered audio tracks.
pub fn scan_directory(root: &Path) -> Result<Vec<Track>> {
    let mut tracks = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());

        let Some(ext) = ext else { continue };
        let is_lofty   = LOFTY_EXTENSIONS.contains(&ext.as_str());
        let is_tracker = tracker::is_tracker_ext(&ext);

        if !is_lofty && !is_tracker {
            continue;
        }

        let file_size = match std::fs::metadata(path) {
            Ok(m) => m.len(),
            Err(e) => {
                warn!("Could not stat {}: {}", path.display(), e);
                continue;
            }
        };

        debug!("Found track: {}", path.display());

        let track = if is_tracker {
            let tmeta = tracker::read_metadata(path);
            Track {
                path: path.to_path_buf(),
                title:          tmeta.title,
                artist:         tmeta.artist,
                album:          String::new(),
                year:           None,
                duration_secs:  None, // populated by TrackerSource at play time
                file_size,
                bitrate_kbps:   None,
                sample_rate_hz: Some(48_000),
                channels:       if tmeta.channels > 0 { Some(tmeta.channels as u8) } else { None },
            }
        } else {
            let meta = read_metadata(path);
            Track {
                path: path.to_path_buf(),
                title:          meta.title,
                artist:         meta.artist,
                album:          meta.album,
                year:           meta.year,
                duration_secs:  meta.duration_secs,
                file_size,
                bitrate_kbps:   meta.bitrate_kbps,
                sample_rate_hz: meta.sample_rate_hz,
                channels:       meta.channels,
            }
        };
        tracks.push(track);
    }

    tracks.sort_by(|a, b| {
        a.artist
            .cmp(&b.artist)
            .then_with(|| a.album.cmp(&b.album))
            .then_with(|| a.title.cmp(&b.title))
    });

    Ok(tracks)
}

struct TrackMeta {
    title: String,
    artist: String,
    album: String,
    year: Option<u32>,
    duration_secs: Option<u32>,
    bitrate_kbps: Option<u32>,
    sample_rate_hz: Option<u32>,
    channels: Option<u8>,
}

// ---------------------------------------------------------------------------
// Tag writing
// ---------------------------------------------------------------------------

/// Write the fields in `edit` back to the audio file's tags.
///
/// Opens the file with lofty, modifies only the `Some` fields on the primary
/// tag (or the first available tag if there is no primary), then saves the
/// file in-place using lofty's format-aware writer.
///
/// lofty preserves all tag fields that are not explicitly overwritten, so
/// existing metadata not mentioned in `edit` is untouched.
///
/// # Errors
///
/// Returns [`AppError::Metadata`] if the file cannot be opened, has no
/// writable tag, or the save fails.
pub fn write_metadata(path: &Path, edit: &TrackEdit) -> Result<()> {
    let mut tagged = Probe::open(path)
        .and_then(|p| p.read())
        .map_err(|e| AppError::Metadata(format!("Cannot open '{}': {e}", path.display())))?;

    // primary_tag_mut returns the format's "canonical" tag (ID3v2 for MP3,
    // iTunes atom for M4A, VorbisComment for FLAC/OGG, etc.).  Fall back to
    // the first available tag if there is no primary.
    // The two-step if/else avoids a double-mutable-borrow that would arise
    // from using `.or_else(|| tagged.first_tag_mut())` in a closure.
    let tag = if tagged.primary_tag().is_some() {
        tagged.primary_tag_mut().unwrap()
    } else {
        tagged.first_tag_mut().ok_or_else(|| {
            AppError::Metadata(format!(
                "No writable tag found in '{}'",
                path.display()
            ))
        })?
    };

    if let Some(ref title)  = edit.title  { tag.set_title(title.clone()); }
    if let Some(ref artist) = edit.artist { tag.set_artist(artist.clone()); }
    if let Some(ref album)  = edit.album  { tag.set_album(album.clone()); }
    if let Some(ref genre)  = edit.genre  { tag.set_genre(genre.clone()); }
    if let Some(year)         = edit.year         { tag.set_year(year); }
    if let Some(track_number) = edit.track_number { tag.set_track(track_number); }

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| AppError::Metadata(format!("Cannot save tags for '{}': {e}", path.display())))
}

fn read_metadata(path: &Path) -> TrackMeta {
    let tagged = match Probe::open(path).and_then(|p| p.read()) {
        Ok(f) => f,
        Err(e) => {
            warn!("Could not read tags for {}: {}", path.display(), e);
            return TrackMeta {
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                year: None,
                duration_secs: None,
                bitrate_kbps: None,
                sample_rate_hz: None,
                channels: None,
            };
        }
    };

    let props = tagged.properties();
    let duration_secs = props.duration().as_secs().try_into().ok();
    let bitrate_kbps = props.audio_bitrate();
    let sample_rate_hz = props.sample_rate();
    let channels = props.channels();

    let tag = tagged.primary_tag();
    let title  = tag.and_then(|t| t.title()).map(|s| s.to_string()).unwrap_or_default();
    let artist = tag.and_then(|t| t.artist()).map(|s| s.to_string()).unwrap_or_default();
    let album  = tag.and_then(|t| t.album()).map(|s| s.to_string()).unwrap_or_default();
    let year   = tag.and_then(|t| t.year());

    TrackMeta { title, artist, album, year, duration_secs, bitrate_kbps, sample_rate_hz, channels }
}
