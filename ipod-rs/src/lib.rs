//! # ipod-rs
//!
//! Pure-Rust library for interacting with all legacy iPod models that mount
//! as USB Mass Storage devices.
//!
//! ## Supported models
//!
//! | Family                | Database    | Layout                    |
//! |-----------------------|-------------|---------------------------|
//! | Classic  (1st–6th gen)| iTunesDB    | `iPod_Control/Music/Fxx/` |
//! | Mini     (1st–2nd gen)| iTunesDB    | `iPod_Control/Music/Fxx/` |
//! | Nano     (1st–5th gen)| iTunesDB    | `iPod_Control/Music/Fxx/` |
//! | Shuffle  (1st–3rd gen)| iTunesSD    | `iPod_Control/Music/`     |
//!
//! iPod touch (all generations) uses the iOS/AFC protocol and is **not**
//! handled by this crate.
//!
//! ## Quick start
//!
//! ```no_run
//! use ipod_rs::{IpodDevice, IpodTrack};
//!
//! let devices = IpodDevice::detect();
//! for dev in &devices {
//!     println!("Found: {} ({:?})", dev.label(), dev.kind());
//! }
//!
//! if let Some(dev) = devices.first() {
//!     let track = IpodTrack {
//!         local_path:     "/home/user/Music/song.mp3".into(),
//!         title:          "My Song".into(),
//!         artist:         "Artist".into(),
//!         album:          "Album".into(),
//!         duration_ms:    210_000,
//!         file_size:      5_242_880,
//!         bitrate_kbps:   128,
//!         sample_rate_hz: 44100,
//!         year:           2006,
//!     };
//!     dev.upload(&track).unwrap();
//! }
//! ```

pub mod detect;
pub mod itunesdb;
pub mod itunessd;

// ---------------------------------------------------------------------------
// Internal utilities
// ---------------------------------------------------------------------------

/// Write `data` to `path` atomically: write to a sibling `.tmp` file first,
/// then rename into place.
///
/// Using a sibling temp file guarantees both paths are on the same filesystem,
/// which is required for `rename(2)` to be atomic.  If the rename fails the
/// temp file is removed so no litter is left behind.
///
/// On Windows `fs::rename` replaces an existing destination atomically on
/// NTFS (Vista+).  If the destination is locked by another process the rename
/// returns an error — the database is untouched, which is the safe outcome.
pub(crate) fn atomic_write(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::fs;

    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("db");
    let tmp = dir.join(format!("{name}.tmp"));

    // Remove any leftover temp file from a previous crash before writing.
    let _ = fs::remove_file(&tmp);

    fs::write(&tmp, data)?;
    fs::rename(&tmp, path).map_err(|e| {
        // Rename failed — clean up the temp file rather than leaving it behind.
        let _ = fs::remove_file(&tmp);
        e
    })
}

pub use detect::{DeviceScanResult, DeviceTrackEntry, FirmwareInfo, IpodDevice, IpodKind, IncompleteEntry, OrphanedFile, UploadResult};

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// IpodTrack — the data the caller supplies about a track
// ---------------------------------------------------------------------------

/// Metadata and source path for a track to be uploaded to an iPod.
#[derive(Debug, Clone)]
pub struct IpodTrack {
    /// Full path to the audio file on the local machine.
    pub local_path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Track duration in milliseconds.
    pub duration_ms: u32,
    /// File size in bytes.
    pub file_size: u64,
    /// Audio bitrate in kbps (e.g. 128, 320). 0 if unknown.
    pub bitrate_kbps: u32,
    /// Sample rate in Hz (e.g. 44100). 0 if unknown.
    pub sample_rate_hz: u32,
    /// Release year (e.g. 2006). 0 if unknown.
    pub year: u32,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum IpodError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database error: {0}")]
    Database(String),

    #[error("Unsupported database version: {0}")]
    UnsupportedVersion(u32),

    #[error("Device not found")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, IpodError>;
