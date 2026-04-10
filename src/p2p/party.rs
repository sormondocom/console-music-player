//! Party Line — democratic group playback.
//!
//! Any trusted peer can nominate a track.  When a simple majority of
//! currently-online trusted peers vote `Yes`, all peers start playing the
//! track at a synchronised UTC timestamp (`start_at = now + 5s`).
//!
//! A nomination expires after 60 seconds if the threshold is not reached.

use std::collections::HashSet;
use std::time::Instant;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::p2p::wire::{PartyVote, RemoteTrack};

// ---------------------------------------------------------------------------
// Nomination
// ---------------------------------------------------------------------------

/// An in-progress vote on a track nomination.
#[derive(Debug, Clone)]
pub struct Nomination {
    pub id: Uuid,
    pub track: RemoteTrack,
    /// Display nickname of the peer who nominated the track.
    pub nominated_by: String,
    /// Wall-clock time this nomination was created (for timeout).
    pub created_at: Instant,
    /// PGP fingerprints of peers who voted Yes.
    pub votes_yes: HashSet<String>,
    /// PGP fingerprints of peers who voted No.
    pub votes_no: HashSet<String>,
}

impl Nomination {
    pub fn new(id: Uuid, track: RemoteTrack, nominated_by: String) -> Self {
        Self {
            id,
            track,
            nominated_by,
            created_at: Instant::now(),
            votes_yes: HashSet::new(),
            votes_no: HashSet::new(),
        }
    }

    /// Seconds remaining before this nomination expires.
    pub fn seconds_remaining(&self) -> u64 {
        const LIFETIME_SECS: u64 = 60;
        LIFETIME_SECS.saturating_sub(self.created_at.elapsed().as_secs())
    }

    pub fn is_expired(&self) -> bool {
        self.seconds_remaining() == 0
    }

    /// Record a vote from a peer.  Ignores duplicate votes.
    pub fn cast_vote(&mut self, voter_fp: &str, vote: &PartyVote) {
        match vote {
            PartyVote::Yes => {
                self.votes_no.remove(voter_fp);
                self.votes_yes.insert(voter_fp.to_string());
            }
            PartyVote::No => {
                self.votes_yes.remove(voter_fp);
                self.votes_no.insert(voter_fp.to_string());
            }
        }
    }

    /// True if `votes_yes > online_peer_count / 2`.
    /// Minimum quorum: 2 online peers (can't pass solo).
    pub fn has_majority(&self, online_peer_count: usize) -> bool {
        if online_peer_count < 2 {
            return false;
        }
        self.votes_yes.len() * 2 > online_peer_count
    }

    /// Total votes cast.
    pub fn vote_count(&self) -> usize {
        self.votes_yes.len() + self.votes_no.len()
    }
}

// ---------------------------------------------------------------------------
// ActiveParty — a vote that passed
// ---------------------------------------------------------------------------

/// A party-line session that is currently active (playing or about to play).
#[derive(Debug, Clone)]
pub struct ActiveParty {
    pub nomination_id: Uuid,
    pub track: RemoteTrack,
    /// The UTC instant at which all peers should begin playback.
    pub start_at: DateTime<Utc>,
    /// Whether this peer's local buffer is ready for playback.
    pub buffer_ready: bool,
    /// Whether playback has started (locally).
    pub started: bool,
}

// ---------------------------------------------------------------------------
// PartyLineState — top-level container
// ---------------------------------------------------------------------------

/// All state for the Party Line feature, held in `App`.
#[derive(Debug)]
pub struct PartyLineState {
    /// Currently active nominations (pending vote).
    pub nominations: Vec<Nomination>,
    /// A party that has passed its vote and is in progress.
    pub active: Option<ActiveParty>,
    /// Index into `nominations` for the PartyLine screen cursor.
    pub selected: usize,
}

impl PartyLineState {
    pub fn new() -> Self {
        Self {
            nominations: Vec::new(),
            active: None,
            selected: 0,
        }
    }

    /// Add or update a nomination.
    pub fn upsert_nomination(&mut self, nom: Nomination) {
        if let Some(existing) = self.nominations.iter_mut().find(|n| n.id == nom.id) {
            *existing = nom;
        } else {
            self.nominations.push(nom);
        }
    }

    /// Remove expired nominations and return how many were pruned.
    pub fn prune_expired(&mut self) -> usize {
        let before = self.nominations.len();
        self.nominations.retain(|n| !n.is_expired());
        // Keep cursor in bounds.
        if !self.nominations.is_empty() {
            self.selected = self.selected.min(self.nominations.len() - 1);
        } else {
            self.selected = 0;
        }
        before - self.nominations.len()
    }

    pub fn focused_nomination(&self) -> Option<&Nomination> {
        self.nominations.get(self.selected)
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_down(&mut self) {
        if !self.nominations.is_empty() {
            self.selected = (self.selected + 1).min(self.nominations.len() - 1);
        }
    }
}
