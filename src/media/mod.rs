//! Unified trait interface for all media types in the library.
//!
//! Every audio item — whether a standard tagged file (MP3, FLAC, OGG, …) or
//! a MOD tracker module — implements [`MediaItem`].  The trait provides a
//! consistent API for display, metadata access, and capability queries so
//! higher-level code never needs to branch on format.
//!
//! # Hierarchy
//!
//! ```text
//! MediaItem                    (core: identity, display, capabilities)
//! └── impl'd by Track          (src/library/mod.rs)
//! ```
//!
//! # Adding a new format
//!
//! 1. Add a variant to [`MediaFormat`] if it represents a new playback family.
//! 2. Add a variant to [`MediaCapability`] for any new operation it supports.
//! 3. Implement [`MediaItem`] on the concrete type — only the nine required
//!    methods need bodies; all display helpers are provided by default.

use std::path::Path;

// ---------------------------------------------------------------------------
// Format classification
// ---------------------------------------------------------------------------

/// Broad playback-family classification for a media item.
///
/// More granular format information is available via [`MediaItem::format_label`],
/// which returns the raw (lower-cased) file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaFormat {
    /// Standard PCM/compressed audio decoded by symphonia/rodio.
    ///
    /// Includes: MP3, M4A, AAC, FLAC, OGG, Opus, WAV, AIFF.
    Standard,
    /// MOD tracker module rendered by libopenmpt.
    ///
    /// Includes: MOD, XM, IT, S3M, MO3, and 20+ legacy formats.
    Tracker,
}

impl MediaFormat {
    /// Short human-readable label (e.g. for UI badges).
    pub fn display_name(self) -> &'static str {
        match self {
            MediaFormat::Standard => "Audio",
            MediaFormat::Tracker  => "Tracker",
        }
    }
}

// ---------------------------------------------------------------------------
// Capability flags
// ---------------------------------------------------------------------------

/// Feature flags indicating which operations a media item supports.
///
/// Use [`MediaItem::supports`] to query a single capability, or
/// [`MediaItem::capabilities`] to inspect the full set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaCapability {
    /// Embedded audio tags (title, artist, album, …) can be read and written
    /// via lofty.  Tracker modules do not support this — their metadata is
    /// stored in the binary module header.
    TagEdit,
    /// The track can be copied to an iPod via the iTunesDB / iTunesSD path.
    IpodTransfer,
    /// Decoded and played back via the standard symphonia/rodio pipeline.
    StandardPlayback,
    /// Rendered via libopenmpt (requires the `tracker` Cargo feature and the
    /// libopenmpt system library).
    TrackerPlayback,
}

// ---------------------------------------------------------------------------
// Core trait
// ---------------------------------------------------------------------------

/// Every media item in the library implements this trait.
///
/// Implementors must provide the nine required methods; all display helpers
/// and capability queries have default implementations built on top of them,
/// so format-specific code is kept to a minimum.
///
/// # Example
///
/// ```no_run
/// use crate::media::{MediaItem, MediaCapability};
///
/// fn show(item: &impl MediaItem) {
///     println!("{}", item.info_line());
///     if item.supports(MediaCapability::TagEdit) {
///         println!("  (tags editable)");
///     }
/// }
/// ```
pub trait MediaItem {
    // -----------------------------------------------------------------------
    // Required — implementors must provide these
    // -----------------------------------------------------------------------

    fn path(&self) -> &Path;

    /// Raw title tag value — may be empty; use [`display_title`] for UI.
    ///
    /// [`display_title`]: MediaItem::display_title
    fn title(&self) -> &str;

    /// Raw artist tag value — may be empty; use [`display_artist`] for UI.
    ///
    /// [`display_artist`]: MediaItem::display_artist
    fn artist(&self) -> &str;

    /// Raw album tag value — may be empty.
    fn album(&self) -> &str;

    fn year(&self) -> Option<u32>;
    fn duration_secs(&self) -> Option<u32>;
    fn file_size(&self) -> u64;

    /// Broad playback-family classification.
    fn format(&self) -> MediaFormat;

    /// The full set of operations this item supports.
    fn capabilities(&self) -> &'static [MediaCapability];

    // -----------------------------------------------------------------------
    // Provided — display helpers with sensible fallbacks
    // -----------------------------------------------------------------------

    /// Title for display: falls back to the filename stem when the tag is empty.
    fn display_title(&self) -> &str {
        let t = self.title();
        if t.is_empty() {
            self.path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unknown>")
        } else {
            t
        }
    }

    /// Artist for display: falls back to `"<unknown artist>"` when the tag is
    /// empty.
    fn display_artist(&self) -> &str {
        let a = self.artist();
        if a.is_empty() { "<unknown artist>" } else { a }
    }

    /// Duration as `"MM:SS"`, or `"--:--"` when unknown.
    fn display_duration(&self) -> String {
        match self.duration_secs() {
            Some(s) => format!("{:02}:{:02}", s / 60, s % 60),
            None => "--:--".to_string(),
        }
    }

    /// Lower-cased file extension (e.g. `"mp3"`, `"xm"`), or `"?"`.
    fn format_label(&self) -> &str {
        self.path()
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("?")
    }

    /// One-line library display string.
    ///
    /// Format: `"Title  ·  Artist  ·  Album (Year)   MM:SS"`.
    /// Sections are omitted when the underlying data is unavailable.
    fn info_line(&self) -> String {
        let mut parts: Vec<String> = vec![self.display_title().to_string()];
        if !self.artist().is_empty() {
            parts.push(self.display_artist().to_string());
        }
        match (self.album().is_empty(), self.year()) {
            (false, Some(y)) => parts.push(format!("{} ({})", self.album(), y)),
            (false, None)    => parts.push(self.album().to_string()),
            (true,  Some(y)) => parts.push(format!("({})", y)),
            (true,  None)    => {}
        }
        format!("{}   {}", parts.join("  ·  "), self.display_duration())
    }

    // -----------------------------------------------------------------------
    // Provided — capability helpers
    // -----------------------------------------------------------------------

    /// Returns `true` if this item supports `cap`.
    fn supports(&self, cap: MediaCapability) -> bool {
        self.capabilities().contains(&cap)
    }
}
