//! File-format verification via magic bytes.
//!
//! Extensions are cheap to forge — a renamed file, a bad rip, a misnamed
//! download.  This module reads the first bytes of each candidate file and
//! confirms they match the format the extension claims.  Files that fail
//! verification are rejected at import time rather than silently misbehaving
//! at play time.
//!
//! Only formats in our supported-extension lists are checked; the logic is
//! intentionally conservative — when in doubt, the file is accepted.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Result of a magic-byte check.
#[derive(Debug, PartialEq, Eq)]
pub enum MagicVerdict {
    /// Magic matches the declared extension — safe to import.
    Ok,
    /// Magic is recognised but belongs to a *different* format.
    Mismatch {
        /// Uppercase extension that the bytes actually indicate.
        detected: &'static str,
    },
    /// Magic is unrecognised for this extension class.  The file may be
    /// corrupt, a stub, or encrypted.
    Unknown,
}

/// Read the first `n` bytes of `path` into a fixed buffer.  Returns a
/// zero-padded buffer if the file is shorter than `n`.
fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

/// Read `n` bytes starting at `offset` within the file.
fn read_at(path: &Path, offset: u64, n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

// ---------------------------------------------------------------------------
// Format detectors
// ---------------------------------------------------------------------------

/// Try to identify a file purely from its magic bytes.
/// Returns the likely uppercase extension, or `None` if unrecognised.
pub fn detect_format(path: &Path) -> Option<&'static str> {
    let head = read_head(path, 12).ok()?;
    if head.len() < 4 { return None; }

    // ID3-tagged MP3
    if head.starts_with(b"ID3") { return Some("MP3"); }
    // Raw MPEG frame sync  (FF Ex / FF Fx)
    if head[0] == 0xFF && (head[1] & 0xE0) == 0xE0 { return Some("MP3"); }
    // FLAC
    if head.starts_with(b"fLaC") { return Some("FLAC"); }
    // Ogg container (OGG, Opus, OGG-encoded Vorbis)
    if head.starts_with(b"OggS") { return Some("OGG"); }
    // MP4/M4A/AAC — `ftyp` box starts at byte 4
    if head.len() >= 8 && &head[4..8] == b"ftyp" { return Some("M4A"); }
    // WAV
    if head.starts_with(b"RIFF") {
        if let Ok(h) = read_head(path, 12) {
            if h.len() >= 12 && &h[8..12] == b"WAVE" { return Some("WAV"); }
        }
    }
    // AIFF / AIFC
    if head.starts_with(b"FORM") {
        if let Ok(h) = read_head(path, 12) {
            if h.len() >= 12 && (&h[8..12] == b"AIFF" || &h[8..12] == b"AIFC") {
                return Some("AIFF");
            }
        }
    }

    // --- Tracker formats ---

    // Impulse Tracker
    if head.starts_with(b"IMPM") { return Some("IT"); }
    // FastTracker 2 XM
    if head.len() >= 17 && &head[..17] == b"Extended Module: " { return Some("XM"); }
    // Scream Tracker 3 — magic at byte 44
    if let Ok(b) = read_at(path, 44, 4) {
        if b == b"SCRM" { return Some("S3M"); }
    }
    // Amiga ProTracker MOD — 4-byte tag at byte 1080
    if let Ok(b) = read_at(path, 1080, 4) {
        if is_mod_magic(&b) { return Some("MOD"); }
    }

    None
}

/// Returns `true` if `tag` is a recognised ProTracker/NoiseTracker/etc. signature.
fn is_mod_magic(tag: &[u8]) -> bool {
    if tag.len() < 4 { return false; }
    matches!(
        tag,
        b"M.K." | b"M!K!" | b"M&K!" | b"N.T." | b"FLT4" | b"FLT8"
        | b"4CHN" | b"6CHN" | b"8CHN"
    ) || (tag[1] == b'C' && tag[2] == b'H' && tag[3] == b'N' && tag[0].is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Verify that the file at `path` with declared (lowercase) extension `ext`
/// actually contains the bytes expected for that format.
///
/// On I/O error the verdict is `Ok` — we don't reject files we couldn't read.
pub fn verify(path: &Path, ext: &str) -> MagicVerdict {
    // Group extensions into families so we can check the right magic.
    let detected = match detect_format(path) {
        Some(d) => d,
        // Cannot detect — could be a very short file or an unsupported sub-format.
        // Accept rather than reject valid files with unusual magic.
        None => return MagicVerdict::Unknown,
    };

    let ext_upper = ext.to_uppercase();

    // Exact match (most common path).
    if detected == ext_upper { return MagicVerdict::Ok; }

    // Equivalent aliases: .aif ↔ .aiff, .opus ↔ .ogg, .m4a ↔ .aac
    if matches!(
        (detected, ext_upper.as_str()),
        ("AIFF", "AIF")  | ("AIF",  "AIFF")
        | ("OGG",  "OPUS") | ("OPUS", "OGG")
        | ("M4A",  "AAC")  | ("AAC",  "M4A")
        // MP4 container files are equivalent to M4A for our purposes
        | ("M4A",  "MP4")  | ("MP4",  "M4A")
    ) {
        return MagicVerdict::Ok;
    }

    MagicVerdict::Mismatch { detected }
}
