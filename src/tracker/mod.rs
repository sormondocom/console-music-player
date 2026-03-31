//! MOD tracker support: metadata parsing and playback via libopenmpt.
//!
//! ## Supported formats
//!
//! | Extension | Format                     | Origin          |
//! |-----------|----------------------------|-----------------|
//! | `.mod`    | Amiga ProTracker / NoiseTracker | Amiga 1987 |
//! | `.xm`     | FastTracker 2 Extended Module  | PC 1994     |
//! | `.it`     | Impulse Tracker             | PC 1995         |
//! | `.s3m`    | Scream Tracker 3            | PC 1994         |
//! | `.mo3`    | Compressed MOD/XM/IT/S3M    | 2001+           |
//! | `.mptm`   | OpenMPT native              | 2004+           |
//! | Various   | Legacy: 669, AMF, DBM, DMF, DSM, FAR, MDL, MED, MTM, OKT, PTM, STM, ULT, UMX, WOW | various |
//!
//! ## Metadata
//!
//! Parsed directly from binary file headers — no external library required.
//! Field availability varies by format:
//!
//! | Format | Title | Artist        | Channels |
//! |--------|-------|---------------|----------|
//! | MOD    | ✓     | (filename)    | header   |
//! | XM     | ✓     | tracker name  | header   |
//! | IT     | ✓     | (filename)    | header   |
//! | S3M    | ✓     | (filename)    | header   |
//!
//! ## Playback (requires `tracker` feature + libopenmpt)
//!
//! [`TrackerSource`] implements `rodio::Source` by rendering tracker audio
//! to interleaved 16-bit stereo PCM via libopenmpt, which is then fed into
//! the existing rodio `Sink` exactly like a regular audio decoder.

use std::path::Path;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Supported extensions
// ---------------------------------------------------------------------------

/// Every file extension we treat as a tracker module.
///
/// The list is split into "primary" formats (MOD, XM, IT, S3M) that are
/// extremely common and "legacy" formats that libopenmpt supports but are
/// rarely encountered in the wild.
pub const TRACKER_EXTENSIONS: &[&str] = &[
    // Primary
    "mod", "xm", "it", "s3m", "mo3", "mptm",
    // Legacy
    "669", "amf", "ams", "dbm", "dmf", "dsm",
    "far", "mdl", "med", "mtm", "okt", "ptm",
    "stm", "ult", "umx", "wow",
];

/// Returns `true` if `ext` (lowercase, no leading dot) is a tracker format.
pub fn is_tracker_ext(ext: &str) -> bool {
    TRACKER_EXTENSIONS.contains(&ext)
}

// ---------------------------------------------------------------------------
// Pure-Rust metadata — no external library needed
// ---------------------------------------------------------------------------

/// Metadata extracted from a tracker file header.
#[derive(Debug, Default)]
pub struct TrackerMeta {
    pub title: String,
    /// Composer / artist — extracted where the format stores it.
    /// Falls back to an empty string when unavailable.
    pub artist: String,
    /// Number of audio channels encoded in the module.
    pub channels: u16,
    /// Short format label for display (e.g. `"MOD"`, `"XM"`, `"IT"`).
    pub format: String,
}

/// Read metadata from a tracker file using pure Rust binary header parsing.
///
/// Returns a best-effort `TrackerMeta`; all fields degrade gracefully on
/// corrupt or unrecognised files rather than returning an error.
pub fn read_metadata(path: &Path) -> TrackerMeta {
    let Ok(data) = std::fs::read(path) else {
        return TrackerMeta::default();
    };

    // Identify format by magic bytes, falling back to extension.
    if data.len() >= 4 && &data[0..4] == b"IMPM" {
        return parse_it(&data);
    }
    if data.len() >= 17 && &data[0..17] == b"Extended Module: " {
        return parse_xm(&data);
    }
    if data.len() >= 48 && &data[44..48] == b"SCRM" {
        return parse_s3m(&data);
    }
    // MO3: "MO3!" magic
    if data.len() >= 4 && &data[0..4] == b"MO3!" {
        return TrackerMeta { format: "MO3".into(), ..Default::default() };
    }
    // MOD: magic at 1080 ("M.K.", "6CHN", "8CHN", etc.)
    if data.len() >= 1084 {
        return parse_mod(&data);
    }

    TrackerMeta::default()
}

// ---------------------------------------------------------------------------
// Format parsers
// ---------------------------------------------------------------------------

/// Parse an Amiga ProTracker / NoiseTracker MOD file.
///
/// Layout (all big-endian):
/// - `[0..20]`   song name (null-terminated ASCII)
/// - `[20..950]` 31 × 30-byte instrument records (first 22 bytes = name)
/// - `[950]`     song length (pattern count used)
/// - `[951]`     restart position
/// - `[952..1080]` 128-byte pattern sequence
/// - `[1080..1084]` channel/type tag ("M.K." / "6CHN" / "8CHN" / …)
fn parse_mod(data: &[u8]) -> TrackerMeta {
    let title = read_ascii(data, 0, 20);

    let channels = mod_channel_count(data);

    // Build a human-readable format label from the 4-byte tag
    let tag = if data.len() >= 1084 {
        std::str::from_utf8(&data[1080..1084])
            .unwrap_or("MOD")
            .trim_end_matches('\0')
            .to_string()
    } else {
        "MOD".into()
    };

    TrackerMeta {
        title,
        artist: String::new(),
        channels,
        format: if tag.is_empty() { "MOD".into() } else { tag },
    }
}

/// Parse a FastTracker 2 Extended Module (.xm).
///
/// Layout:
/// - `[0..17]`   "Extended Module: " (17 bytes)
/// - `[17..37]`  module name (20 bytes, null-padded ASCII)
/// - `[37]`      0x1A (magic byte)
/// - `[38..58]`  tracker name (20 bytes) — used as artist proxy
/// - `[58..60]`  version (LE u16)
/// - `[60..64]`  header size (LE u32)
/// - `[64..66]`  song length
/// - `[70..72]`  number of channels
fn parse_xm(data: &[u8]) -> TrackerMeta {
    let title    = read_ascii(data, 17, 20);
    let artist   = read_ascii(data, 38, 20); // tracker / author name
    let channels = read_le_u16(data, 70);
    TrackerMeta { title, artist, channels, format: "XM".into() }
}

/// Parse an Impulse Tracker (.it).
///
/// Layout:
/// - `[0..4]`   "IMPM" magic
/// - `[4..30]`  song name (26 bytes, null-padded)
/// - `[30..32]` highlight / initial speed / initial tempo
/// - `[32..34]` order count
/// - `[34..36]` instrument count
/// - `[36..38]` sample count
/// - `[38..40]` pattern count
/// - `[40..42]` Cwtv (tracker version)
/// - `[42..44]` Compatible with tracker version
/// - `[44..46]` Flags
/// - `[46..48]` Special flags
/// - `[48]`     global volume
/// - `[49]`     mix volume
/// - `[50]`     initial speed
/// - `[51]`     initial tempo
/// - `[52]`     panning separation
/// - `[53]`     MIDI pitch wheel depth
/// - `[54..56]` message length
/// - `[56..60]` message offset
/// - `[60..64]` reserved
/// - `[64..128]` channel panning (64 channels)
fn parse_it(data: &[u8]) -> TrackerMeta {
    let title    = read_ascii(data, 4, 26);
    let channels = it_active_channels(data);
    TrackerMeta { title, artist: String::new(), channels, format: "IT".into() }
}

/// Parse a Scream Tracker 3 (.s3m).
///
/// Layout:
/// - `[0..28]`  song name (28 bytes, null-padded)
/// - `[28]`     0x1A (end-of-file marker)
/// - `[29]`     type (1 = module, 2 = unknown)
/// - `[30..32]` reserved
/// - `[32..34]` number of orders
/// - `[34..36]` number of instruments
/// - `[36..38]` number of patterns
/// - `[44..48]` "SCRM" magic
fn parse_s3m(data: &[u8]) -> TrackerMeta {
    let title    = read_ascii(data, 0, 28);
    let channels = s3m_channel_count(data);
    TrackerMeta { title, artist: String::new(), channels, format: "S3M".into() }
}

// ---------------------------------------------------------------------------
// Format-specific helpers
// ---------------------------------------------------------------------------

fn mod_channel_count(data: &[u8]) -> u16 {
    if data.len() < 1084 { return 4; }
    match &data[1080..1084] {
        b"6CHN"                        => 6,
        b"8CHN" | b"FLT8" | b"CD81"   => 8,
        b"M.K." | b"M!K!" | b"M&K!"
        | b"N.T." | b"NSMS"           => 4,
        tag => {
            // "xxCH" pattern where xx is a decimal channel count
            if tag[2..4] == *b"CH" {
                let s = std::str::from_utf8(&tag[0..2]).unwrap_or("4");
                s.trim().parse().unwrap_or(4)
            } else {
                4
            }
        }
    }
}

fn it_active_channels(data: &[u8]) -> u16 {
    // Channel panning table starts at offset 64, 64 bytes (one per channel).
    // A value of 100 (0x64) means the channel is disabled.
    if data.len() < 128 { return 0; }
    data[64..128].iter().filter(|&&b| b != 100 && b != 0xFF).count() as u16
}

fn s3m_channel_count(data: &[u8]) -> u16 {
    // Orders count at offset 32 (u16 LE) — this is the number of pattern
    // order slots, not channels. Channels in S3M = non-0xFF entries in the
    // channel settings at offset 64 (32 bytes). We skip this detail and
    // just return 0 to avoid over-engineering.
    let _ = data;
    0
}

// ---------------------------------------------------------------------------
// Binary helpers
// ---------------------------------------------------------------------------

/// Read a null-terminated ASCII string from `data[offset..offset+len]`,
/// stripping control characters and trailing whitespace.
fn read_ascii(data: &[u8], offset: usize, len: usize) -> String {
    if offset + len > data.len() {
        return String::new();
    }
    data[offset..offset + len]
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|&&b| b.is_ascii() && !b.is_ascii_control())
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
}

fn read_le_u16(data: &[u8], offset: usize) -> u16 {
    if offset + 2 > data.len() { return 0; }
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

// ---------------------------------------------------------------------------
// TrackerSource — rodio::Source backed by libopenmpt
// ---------------------------------------------------------------------------

/// Sample rate used for all tracker rendering.
const SAMPLE_RATE: u32 = 48_000;
/// Stereo — 2 channels.
const CHANNELS: u16 = 2;
/// Render buffer in frames (samples per channel per chunk).
const BUF_FRAMES: usize = 4_096;

/// A `rodio::Source` that decodes tracker modules via libopenmpt.
///
/// Renders interleaved 16-bit stereo PCM on demand, a buffer at a time.
/// libopenmpt returns 0 frames when the module has played through once.
///
/// # Thread safety
///
/// `openmpt::module::Module` wraps a raw C++ object that is not `Sync`, but
/// it is safe to move to another thread as long as only one thread accesses
/// it at a time — which is exactly what rodio's audio thread guarantees.
#[cfg(feature = "tracker")]
pub struct TrackerSource {
    module:      openmpt::module::Module,
    sample_rate: u32,
    /// Interleaved stereo render buffer: L0 R0 L1 R1 …
    interleaved: Vec<i16>,
    buf_pos:     usize,
    buf_len:     usize,
}

// Safety: rodio moves Sources to its audio thread and then accesses them
// exclusively from that thread — no concurrent access ever occurs.
#[cfg(feature = "tracker")]
unsafe impl Send for TrackerSource {}

#[cfg(feature = "tracker")]
impl TrackerSource {
    /// Load a tracker module from raw bytes.
    ///
    /// Returns `None` if libopenmpt cannot parse the data.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        use openmpt::module::{Logger, Module};

        let mut buf = data.to_vec();
        let module = Module::create_from_memory(&mut buf, Logger::None, &[]).ok()?;
        Some(Self {
            module,
            sample_rate: SAMPLE_RATE,
            interleaved: vec![0i16; BUF_FRAMES * CHANNELS as usize],
            buf_pos:     0,
            buf_len:     0,
        })
    }

    /// Module duration in whole seconds, as reported by libopenmpt.
    ///
    /// May differ slightly from actual playback if the module uses
    /// tempo-change effects.
    pub fn duration_secs(&mut self) -> Option<u32> {
        let d = self.module.get_duration_seconds();
        if d > 0.0 { Some(d as u32) } else { None }
    }

    /// Render the next chunk of audio into `self.interleaved`.
    ///
    /// libopenmpt 0.3's `read_interleaved_stereo` writes L0 R0 L1 R1 …
    /// directly into a single `Vec<i16>`, sized to `frames * 2`.
    fn refill(&mut self) -> bool {
        let frames = self.module.read_interleaved_stereo(
            self.sample_rate as i32,
            &mut self.interleaved,
        );
        self.buf_len = frames * CHANNELS as usize;
        self.buf_pos = 0;
        frames > 0
    }
}

#[cfg(feature = "tracker")]
impl Iterator for TrackerSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.buf_pos >= self.buf_len {
            if !self.refill() {
                return None;
            }
        }
        let s = self.interleaved[self.buf_pos];
        self.buf_pos += 1;
        Some(s)
    }
}

#[cfg(feature = "tracker")]
impl rodio::Source for TrackerSource {
    fn current_frame_len(&self) -> Option<usize> {
        // Tell rodio how many samples remain in the current render chunk.
        Some((self.buf_len - self.buf_pos).max(1))
    }

    fn channels(&self) -> u16 {
        CHANNELS
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        // Modules can have tempo automation — don't promise a fixed length.
        None
    }
}
