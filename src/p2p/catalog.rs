//! Builds a `Vec<RemoteTrack>` from the local `Library` for broadcast.
//!
//! Converts local `Track` structs into `RemoteTrack` wire types:
//!   - Assigns stable UUIDv5 content-addressed IDs
//!   - Maps file extensions to `RemoteFormat`
//!   - Omits filesystem paths entirely
//!
//! Also provides `build_path_map()` for the serving side — a separate mapping
//! from track UUID to local `PathBuf`, never transmitted over the wire.

use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use crate::library::Library;
use crate::media::MediaItem;
use crate::p2p::wire::{RemoteFormat, RemoteTrack};

/// Build a `HashMap<Uuid, PathBuf>` mapping each track's content-addressed ID
/// to its local file path.  This mapping is **never** transmitted over the
/// wire — it stays in the node so it can serve file bytes on `TrackRequest`.
pub fn build_path_map(library: &Library) -> HashMap<Uuid, PathBuf> {
    library
        .tracks
        .iter()
        .map(|track| {
            let id = RemoteTrack::compute_id(
                track.display_artist(),
                &track.album,
                track.display_title(),
                track.file_size,
            );
            (id, track.path.clone())
        })
        .collect()
}

/// Build the full catalog from the local library for broadcast to peers.
pub fn build_catalog(library: &Library) -> Vec<RemoteTrack> {
    library
        .tracks
        .iter()
        .map(|track| {
            let ext = track.path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();

            let format = RemoteFormat::from_ext(&ext);

            RemoteTrack {
                id: RemoteTrack::compute_id(
                    track.display_artist(),
                    &track.album,
                    track.display_title(),
                    track.file_size,
                ),
                title:          track.display_title().to_string(),
                artist:         track.display_artist().to_string(),
                album:          track.album.clone(),
                year:           track.year,
                duration_secs:  track.duration_secs,
                file_size:      track.file_size,
                bitrate_kbps:   track.bitrate_kbps,
                sample_rate_hz: track.sample_rate_hz,
                channels:       track.channels,
                format,
                content_hash:   None,
                owner_fp:       String::new(),
                owner_nick:     String::new(),
            }
        })
        .collect()
}
