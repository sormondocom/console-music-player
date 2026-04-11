//! P2P music library sharing — beta feature.
//!
//! Activated by pressing `p` → `2` → `p` within 2 seconds.
//!
//! Architecture overview:
//! - [`P2pHandle`]  — UI-facing channel pair; held in `App::p2p_node`
//! - [`P2pCommand`] — commands sent from the UI to the P2P node
//! - [`P2pEvent`]   — events emitted from the P2P node to the UI
//! - [`P2pBufferState`] — current state of an in-progress remote track buffer
//! - [`Toast`]      — transient non-modal notification

pub mod catalog;
pub mod crypto;
pub mod identity;
pub mod keystore;
pub mod lan_beacon;
pub mod network;
pub mod node;
pub mod party;
pub mod transfer;
pub mod trust;
pub mod wire;

use std::time::Instant;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::p2p::trust::NodeInfo;
use crate::p2p::wire::{PartyVote, RemoteTrack};

// ---------------------------------------------------------------------------
// Commands  (UI → node)
// ---------------------------------------------------------------------------

/// Commands the UI sends to the background P2P node.
pub enum P2pCommand {
    // ── Trust management ──────────────────────────────────────────────────
    /// Broadcast our public key to the room.
    AnnounceKey,
    /// Move a pending key to Trusted.
    ApproveKey(String),
    /// Move a pending/trusted key to Rejected.
    DenyKey(String),
    /// Request a fresh snapshot of the peer list.
    GetPeerList,
    /// Gracefully disconnect from the P2P network.
    Disconnect,

    // ── Library sharing ───────────────────────────────────────────────────
    /// Broadcast a `CatalogPresence` (track count) to all peers and update
    /// the node's local catalog (used to respond to `CatalogRequest`).
    AnnounceLibrary(Vec<RemoteTrack>),
    /// Register local file paths so the node can serve tracks on request.
    /// Send alongside (or just after) `AnnounceLibrary`.
    SetLocalPaths(std::collections::HashMap<uuid::Uuid, std::path::PathBuf>),
    /// Request the full catalog from a specific trusted peer.
    RequestCatalog { peer_fp: String },
    /// Explicitly dial a peer by multiaddr string (for internet peers).
    /// The peer still must be approved before any data is exchanged.
    ConnectPeer { addr: String },

    // ── Track streaming ───────────────────────────────────────────────────
    /// Request a specific track from a peer.
    RequestTrack { track_id: Uuid, peer_fp: String },
    /// Accept an inbound track request (we are the server).
    AcceptTrackRequest { transfer_id: Uuid },
    /// Decline an inbound track request.
    DeclineTrackRequest { transfer_id: Uuid },

    // ── Party line ────────────────────────────────────────────────────────
    /// Nominate a track for group playback.
    NominateTrack(RemoteTrack),
    /// Cast a vote on an active nomination.
    CastVote { nomination_id: Uuid, vote: PartyVote },
}

// ---------------------------------------------------------------------------
// Events  (node → UI)
// ---------------------------------------------------------------------------

/// Events the P2P node emits to the UI via `P2pHandle::events`.
pub enum P2pEvent {
    // ── Peer lifecycle ────────────────────────────────────────────────────
    /// A new peer has announced their key; awaiting user approval.
    PeerApprovalRequired { fingerprint: String, nickname: String },
    /// The user approved a pending peer.
    PeerTrusted { fingerprint: String, nickname: String },
    /// A peer went offline or was rejected.
    PeerOffline { fingerprint: String, nickname: String },
    /// Fresh snapshot of all known peers (response to `GetPeerList`).
    PeerListSnapshot(Vec<NodeInfo>),

    // ── Library sharing ───────────────────────────────────────────────────
    /// A trusted peer sent us their full (or partial) catalog.
    RemoteCatalogReceived {
        peer_fp: String,
        peer_nick: String,
        tracks: Vec<RemoteTrack>,
    },

    // ── Track streaming ───────────────────────────────────────────────────
    /// A peer is requesting one of our tracks (we must accept or decline).
    InboundTrackRequest {
        transfer_id: Uuid,
        track: RemoteTrack,
        requester_fp: String,
    },
    /// Progress update while buffering a remote track.
    TrackBufferProgress {
        transfer_id: Uuid,
        received: u64,
        total: u64,
    },
    /// The in-memory buffer is complete; bytes are ready for playback.
    /// The bytes are wrapped in `Zeroizing` so they are wiped from memory
    /// as soon as the `Vec` is dropped after decoding.
    TrackBufferReady {
        transfer_id: Uuid,
        bytes: Zeroizing<Vec<u8>>,
        track: RemoteTrack,
    },
    /// A transfer failed (peer offline, integrity check failed, etc.).
    TrackTransferFailed {
        transfer_id: Uuid,
        reason: String,
    },

    // ── Party line ────────────────────────────────────────────────────────
    /// A peer nominated a track for group playback.
    TrackNominated {
        nomination_id: Uuid,
        track: RemoteTrack,
        nominated_by: String,
    },
    /// A peer cast a vote on an active nomination.
    VoteReceived {
        nomination_id: Uuid,
        voter_fp: String,
        vote: PartyVote,
    },
    /// A nomination passed — broadcast `start_at` to all peers.
    PartyLinePassed {
        nomination_id: Uuid,
        track: RemoteTrack,
        start_at: DateTime<Utc>,
    },
    /// A nomination expired without reaching majority.
    PartyLineFailed { nomination_id: Uuid },

    // ── Misc ──────────────────────────────────────────────────────────────
    /// Informational message (cyan toast).
    Info(String),
    /// Non-fatal warning (logged as a yellow toast).
    Warning(String),
    /// Our own listen addresses changed (new port bound, etc.).
    /// The strings are human-readable multiaddrs suitable for sharing.
    ListenAddrsUpdated(Vec<String>),
}

// ---------------------------------------------------------------------------
// P2pHandle — UI-facing handle to the background node
// ---------------------------------------------------------------------------

/// Held by `App` when P2P is active.  Dropping this handle signals the
/// background node to disconnect.
pub struct P2pHandle {
    pub commands: mpsc::UnboundedSender<P2pCommand>,
    pub events:   mpsc::UnboundedReceiver<P2pEvent>,
    /// Local peer nickname (for display).
    pub nickname: String,
    /// Local PGP fingerprint (hex).
    pub fingerprint: String,
}

impl P2pHandle {
    /// Create a linked (handle, node-side) channel pair.
    pub fn channel(nickname: String, fingerprint: String) -> (Self, NodeChannels) {
        let (cmd_tx, cmd_rx)     = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let handle = Self {
            commands: cmd_tx,
            events: event_rx,
            nickname,
            fingerprint,
        };
        let channels = NodeChannels { cmd_rx, event_tx };
        (handle, channels)
    }

    /// Send a command, ignoring errors (node may have shut down).
    pub fn send(&self, cmd: P2pCommand) {
        let _ = self.commands.send(cmd);
    }

    /// Drain all pending events without blocking.
    pub fn poll(&mut self) -> Vec<P2pEvent> {
        let mut events = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            events.push(e);
        }
        events
    }
}

/// The node-side end of the channel pair.
pub struct NodeChannels {
    pub cmd_rx:   mpsc::UnboundedReceiver<P2pCommand>,
    pub event_tx: mpsc::UnboundedSender<P2pEvent>,
}

// ---------------------------------------------------------------------------
// P2pBufferState — inline player-pane state
// ---------------------------------------------------------------------------

/// Describes the current remote-track buffering/playback state, used by the
/// player pane renderer to transform the gauge and status line.
#[derive(Debug, Default)]
pub enum P2pBufferState {
    /// No remote track activity.
    #[default]
    Idle,
    /// Request sent; waiting for the remote peer to respond.
    Requesting {
        track_id: Uuid,
        peer_nick: String,
    },
    /// Chunks arriving; downloading into the in-memory buffer.
    Buffering {
        transfer_id: Uuid,
        peer_nick: String,
        received: u64,
        total: u64,
        /// True when no chunk has arrived for more than `STALL_THRESHOLD`.
        stalled: bool,
        stalled_since: Option<Instant>,
        last_chunk_at: Instant,
    },
    /// Buffer complete; playing from in-memory `Cursor<Vec<u8>>`.
    Playing {
        peer_nick: String,
    },
}

impl P2pBufferState {
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
}

// ---------------------------------------------------------------------------
// Toast — transient non-modal notification
// ---------------------------------------------------------------------------

/// Severity level of a toast notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastLevel {
    /// Cyan — informational (peer connected, transfer complete, etc.)
    Info,
    /// Yellow — warning (stall resolved, slow transfer, etc.)
    Warning,
    /// Red — error (integrity failure, peer offline, etc.)
    /// Error toasts persist until explicitly dismissed (`Esc`).
    Error,
}

/// A brief non-modal notification rendered in the bottom-right corner.
#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub level: ToastLevel,
    /// When this toast should auto-dismiss.  `None` = persist until dismissed.
    pub expires_at: Option<Instant>,
}

impl Toast {
    pub fn info(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            level: ToastLevel::Info,
            expires_at: Some(Instant::now() + std::time::Duration::from_secs(4)),
        }
    }

    pub fn warning(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            level: ToastLevel::Warning,
            expires_at: Some(Instant::now() + std::time::Duration::from_secs(6)),
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
            level: ToastLevel::Error,
            expires_at: None, // persists until dismissed
        }
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at.map(|t| Instant::now() >= t).unwrap_or(false)
    }
}
