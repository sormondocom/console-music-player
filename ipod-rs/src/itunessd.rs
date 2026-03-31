//! iTunesSD binary format reader/writer for iPod shuffle (1st–3rd gen).
//!
//! ## Format overview
//!
//! iTunesSD is a simple big-endian binary format consisting of an 18-byte
//! header followed by a flat array of fixed-size 558-byte track records.
//!
//! ```text
//! Header (18 bytes):
//!   [0..3]  track count — 3-byte big-endian unsigned int
//!   [3..6]  0x01 0x06 0x00
//!   [6..18] reserved zeros
//!
//! Track record (558 bytes):
//!   [0]     start-position high byte (0)
//!   [1]     start-position low byte  (0)
//!   [2]     volume (0–100)
//!   [3]     file type: 1=MP3  2=AAC  4=WAV  5=AIFF
//!   [4]     don't-skip flag (0 = shuffle normally)
//!   [5]     remember-bookmark flag (0)
//!   [6]     skip-when-shuffling flag (0)
//!   [7..27] reserved zeros
//!   [27..]  iPod-relative path, null-terminated Mac Roman string
//! ```
//!
//! ## References
//! - <http://shuffle.xfeh.net/formats/itunessd.html>

use std::fs;
use std::path::Path;

use tracing::info;

use crate::{IpodError, IpodTrack, Result};

const SD_HEADER_LEN: usize = 18;
const SD_TRACK_LEN: usize = 558;
const SD_PATH_OFFSET: usize = 27;
const SD_PATH_MAX: usize = SD_TRACK_LEN - SD_PATH_OFFSET; // 531 bytes incl. null terminator

/// Append a single track entry to an iTunesSD file.
///
/// If the file does not exist a fresh one is created.  On success the header
/// track count is incremented and the new record is appended.
///
/// `ext` should be the lowercase file extension without the leading dot
/// (e.g. `"mp3"`, `"m4a"`).
pub fn append_track(
    sd_path: &Path,
    ipod_rel_path: &str,
    ext: &str,
    track: &IpodTrack,
) -> Result<()> {
    // Read existing file or build a fresh 18-byte header
    let mut data = if sd_path.exists() {
        fs::read(sd_path)?
    } else {
        let mut h = vec![0u8; SD_HEADER_LEN];
        // Bytes 3–5: signature 0x01 0x06 0x00
        h[3] = 0x01;
        h[4] = 0x06;
        h
    };

    if data.len() < SD_HEADER_LEN {
        return Err(IpodError::Database("iTunesSD header too short".into()));
    }

    // Current track count encoded as 3-byte big-endian
    let count = (u32::from(data[0]) << 16) | (u32::from(data[1]) << 8) | u32::from(data[2]);

    // Build the 558-byte track record
    let mut record = vec![0u8; SD_TRACK_LEN];
    record[2] = 100; // volume: full
    record[3] = ext_to_filetype(ext);
    // bytes [4..7] stay 0 (don't-skip=0, no-bookmark=0, include-in-shuffle=0)

    // Path field: null-terminated, at most SD_PATH_MAX bytes
    let path_bytes = ipod_rel_path.as_bytes();
    let copy_len = path_bytes.len().min(SD_PATH_MAX - 1); // leave room for null
    record[SD_PATH_OFFSET..SD_PATH_OFFSET + copy_len].copy_from_slice(&path_bytes[..copy_len]);
    // record[SD_PATH_OFFSET + copy_len] is already 0 (null terminator)

    // Append the record and update the count
    data.extend_from_slice(&record);
    let new_count = count + 1;
    data[0] = ((new_count >> 16) & 0xFF) as u8;
    data[1] = ((new_count >> 8) & 0xFF) as u8;
    data[2] = (new_count & 0xFF) as u8;

    // Ensure parent directory exists and write back atomically.
    if let Some(parent) = sd_path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::atomic_write(sd_path, &data)?;

    info!(
        "iTunesSD: {} tracks total, added '{}'",
        new_count,
        track.title
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Map a lowercase file extension to the iTunesSD file-type byte.
fn ext_to_filetype(ext: &str) -> u8 {
    match ext {
        "mp3" => 1,
        "aac" | "m4a" | "m4p" | "m4b" => 2,
        "wav" => 4,
        "aiff" | "aif" => 5,
        _ => 1, // default: MP3
    }
}
