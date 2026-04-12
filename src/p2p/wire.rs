//! Wire types for the P2P music sharing protocol.
//!
//! `RemoteTrack` is the safe-to-transmit representation of a track owned by
//! a remote peer — it contains no local filesystem paths.  The `MusicMessage`
//! / `MusicKind` types define the full gossipsub message envelope.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::p2p::trust::NodeStatus;

// ---------------------------------------------------------------------------
// RemoteFormat — file format hint without path exposure
// ---------------------------------------------------------------------------

/// Audio format of a remote track, transmitted instead of a file extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteFormat {
    Mp3,
    Flac,
    Ogg,
    Aac,
    M4a,
    Wav,
    Opus,
    /// Tracker module: inner string is the extension ("xm", "mod", "it", "s3m", …).
    Tracker(String),
    /// Fallback: raw lowercase extension for formats not listed above.
    Unknown(String),
}

impl RemoteFormat {
    /// Derive a `RemoteFormat` from a file extension string (lowercase).
    pub fn from_ext(ext: &str) -> Self {
        match ext {
            "mp3"  => Self::Mp3,
            "flac" => Self::Flac,
            "ogg"  => Self::Ogg,
            "aac"  => Self::Aac,
            "m4a"  => Self::M4a,
            "wav"  => Self::Wav,
            "opus" => Self::Opus,
            "xm" | "mod" | "it" | "s3m" | "669" | "amf" | "ams"
            | "dbm" | "dmf" | "dsm" | "far" | "mdl" | "med" | "mtm"
            | "okt" | "ptm" | "stm" | "ult" | "umx" | "wow" | "gdm" => {
                Self::Tracker(ext.to_string())
            }
            other  => Self::Unknown(other.to_string()),
        }
    }

    /// Short display label (e.g. "FLAC", "MP3", "XM").
    pub fn label(&self) -> String {
        match self {
            Self::Mp3             => "MP3".into(),
            Self::Flac            => "FLAC".into(),
            Self::Ogg             => "OGG".into(),
            Self::Aac             => "AAC".into(),
            Self::M4a             => "M4A".into(),
            Self::Wav             => "WAV".into(),
            Self::Opus            => "OPUS".into(),
            Self::Tracker(ext)    => ext.to_uppercase(),
            Self::Unknown(ext)    => ext.to_uppercase(),
        }
    }
}

// ---------------------------------------------------------------------------
// RemoteTrack — the core wire type
// ---------------------------------------------------------------------------

/// A track owned by a remote peer, safe to transmit over the network.
///
/// The local filesystem `path` is deliberately absent.  The `id` field is a
/// stable UUIDv5 derived from `artist|album|title|file_size`, allowing the
/// receiver to detect duplicates without revealing paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteTrack {
    /// Stable content-addressed ID: UUIDv5(artist|album|title|file_size).
    /// Used as the transfer request handle — never a filesystem path.
    pub id: Uuid,

    // ── Display metadata ──────────────────────────────────────────────────
    pub title:          String,
    pub artist:         String,
    pub album:          String,
    pub year:           Option<u32>,
    pub duration_secs:  Option<u32>,
    pub file_size:      u64,
    pub bitrate_kbps:   Option<u32>,
    pub sample_rate_hz: Option<u32>,
    pub channels:       Option<u8>,

    // ── Protocol fields ───────────────────────────────────────────────────
    /// Format hint so the receiver knows what decoder to use.
    pub format: RemoteFormat,

    /// SHA-256 of the raw file bytes (hex string).
    /// Dual purpose: post-transfer integrity check AND cross-peer dedup.
    /// `None` if the owner chose not to pre-compute it.
    pub content_hash: Option<String>,

    // ── Local-only fields (never serialised) ─────────────────────────────
    /// PGP fingerprint of the peer who owns this track.
    /// Set by the receiver when building the merged library view.
    #[serde(skip)]
    pub owner_fp: String,

    /// Display nickname of the owning peer.
    #[serde(skip)]
    pub owner_nick: String,
}

impl RemoteTrack {
    /// Compute the stable UUIDv5 ID for a track given its key fields.
    ///
    /// Uses the OID namespace so the IDs are deterministic across peers:
    /// the same track on two different machines gets the same UUID.
    pub fn compute_id(artist: &str, album: &str, title: &str, file_size: u64) -> Uuid {
        let key = format!("{artist}|{album}|{title}|{file_size}");
        Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes())
    }

    /// One-line display string for the TUI list.
    pub fn info_line(&self) -> String {
        let dur = self.duration_secs
            .map(|s| format!("  {:02}:{:02}", s / 60, s % 60))
            .unwrap_or_default();
        format!(
            "{}  ·  {}{}",
            self.title,
            self.artist,
            dur,
        )
    }
}

impl Default for RemoteFormat {
    fn default() -> Self { Self::Unknown(String::new()) }
}

impl Default for RemoteTrack {
    fn default() -> Self {
        Self {
            id:            Uuid::nil(),
            title:         String::new(),
            artist:        String::new(),
            album:         String::new(),
            year:          None,
            duration_secs: None,
            file_size:     0,
            bitrate_kbps:  None,
            sample_rate_hz: None,
            channels:      None,
            format:        RemoteFormat::default(),
            content_hash:  None,
            owner_fp:      String::new(),
            owner_nick:    String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Party line vote
// ---------------------------------------------------------------------------

/// A vote cast in a party-line nomination round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartyVote {
    Yes,
    No,
}

// ---------------------------------------------------------------------------
// MusicKind — all gossipsub message variants
// ---------------------------------------------------------------------------

/// The payload of a signed music-network gossipsub message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum MusicKind {
    // ── Trust management (mirrors pgp-chat) ──────────────────────────────
    /// Peer announces their PGP public key and nickname.
    AnnounceKey {
        public_key_armored: String,
        nickname: String,
    },
    /// Periodic heartbeat: peer is online and announces their status.
    StatusAnnounce {
        status: NodeStatus,
    },
    /// Peer revokes their own identity key.
    Revoke {
        fingerprint: String,
    },

    // ── Library catalog ───────────────────────────────────────────────────
    /// Tiny gossipsub broadcast: "I have N tracks, last sync at T."
    /// Receivers use this to decide whether to request a full catalog.
    CatalogPresence {
        track_count: u32,
    },
    /// Direct request to a specific peer: "send me your catalog."
    CatalogRequest,
    /// Response to a `CatalogRequest` — full catalog, possibly paginated.
    CatalogResponse {
        tracks: Vec<RemoteTrack>,
        /// Page index (0-based).
        page: u32,
        /// Total number of pages.
        total_pages: u32,
        /// Exact total track count across all pages — receiver flushes when
        /// accumulated tracks reach this number, regardless of page order.
        total_tracks: u32,
    },

    // ── Track streaming (3-way handshake) ────────────────────────────────
    /// Receiver requests a specific track by content-addressed ID.
    TrackRequest {
        track_id: Uuid,
    },
    /// Sender accepts and announces the transfer parameters.
    TrackOffer {
        transfer_id: Uuid,
        track: RemoteTrack,
        total_chunks: u32,
    },
    /// One encrypted chunk of audio data.
    TrackChunk {
        transfer_id: Uuid,
        index: u32,
        total: u32,
        /// PGP-encrypted chunk bytes (encrypted to requester's ECDH subkey).
        encrypted_data: Vec<u8>,
    },
    /// All chunks sent; receiver should verify SHA-256 and assemble.
    TrackComplete {
        transfer_id: Uuid,
        /// Lowercase hex SHA-256 of the plaintext audio bytes.
        sha256: String,
    },
    /// Sender declines a transfer request.
    TrackDecline {
        transfer_id: Uuid,
        reason: String,
    },

    // ── Party line ────────────────────────────────────────────────────────
    /// Peer nominates a track for group playback.
    PartyNominate {
        nomination_id: Uuid,
        track: RemoteTrack,
    },
    /// Peer casts a vote on an active nomination.
    PartyVote {
        nomination_id: Uuid,
        vote: PartyVote,
    },
    /// Nomination passed — all peers start playback at `start_at`.
    /// Peers missing the track should begin buffering immediately.
    PartyStart {
        nomination_id: Uuid,
        /// UTC timestamp at which all peers should begin playback.
        /// Set to `Utc::now() + 5s` to give peers time to buffer.
        start_at: DateTime<Utc>,
    },
}

// ---------------------------------------------------------------------------
// MusicMessage — signed wire envelope
// ---------------------------------------------------------------------------

/// A signed music-network message.  The signature covers the serialised
/// `payload` bytes using the sender's EdDSA primary key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicMessage {
    /// UUIDv4 — globally unique message identifier, used for dedup.
    pub id: Uuid,
    /// Gossipsub topic string (e.g. `"cmp-p2p-v1"`).
    pub room: String,
    /// PGP fingerprint of the sender (hex, lowercase).
    pub sender_fp: String,
    /// Display nickname of the sender.
    pub sender_nick: String,
    /// UTC send timestamp.
    pub timestamp: DateTime<Utc>,
    /// The actual payload.
    pub kind: MusicKind,
}

/// A `MusicMessage` bundled with its detached EdDSA signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedMusicMessage {
    pub message: MusicMessage,
    /// Detached PGP signature over `serde_json::to_vec(&message)`.
    pub signature: Vec<u8>,
}
