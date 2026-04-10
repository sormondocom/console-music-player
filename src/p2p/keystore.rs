//! In-memory peer key store with trust-bucket management.
//!
//! Copied from pgp-chat-core/src/chat/keystore.rs.
//! Only change: `use crate::p2p::trust::TrustState` instead of `crate::chat::trust`.

use std::collections::{HashMap, HashSet};

use libp2p::PeerId;
use pgp::composed::SignedPublicKey;

use crate::p2p::trust::TrustState;

// ---------------------------------------------------------------------------
// Storage types
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct PendingEntry {
    peer_id: PeerId,
    key:     SignedPublicKey,
    nick:    String,
}

/// Maps `libp2p::PeerId` ↔ PGP fingerprint ↔ `SignedPublicKey`,
/// split into four trust buckets.
#[derive(Default)]
pub struct PeerKeyStore {
    trusted:  HashMap<String, SignedPublicKey>,
    pending:  HashMap<String, PendingEntry>,
    deferred: HashMap<String, PendingEntry>,
    rejected: HashSet<String>,
    peer_map: HashMap<PeerId, String>,
}

impl PeerKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Insertion
    // -----------------------------------------------------------------------

    pub fn insert_pending(
        &mut self,
        peer_id: PeerId,
        fingerprint: String,
        key: SignedPublicKey,
        nick: String,
    ) -> bool {
        if self.is_known(&fingerprint) { return false; }
        self.pending.insert(fingerprint, PendingEntry { peer_id, key, nick });
        true
    }

    pub fn insert_deferred(
        &mut self,
        peer_id: PeerId,
        fingerprint: String,
        key: SignedPublicKey,
        nick: String,
    ) -> bool {
        if self.is_known(&fingerprint) { return false; }
        self.deferred.insert(fingerprint, PendingEntry { peer_id, key, nick });
        true
    }

    // -----------------------------------------------------------------------
    // Trust management
    // -----------------------------------------------------------------------

    pub fn approve(&mut self, fingerprint: &str) -> Option<String> {
        let entry = self.pending.remove(fingerprint)
            .or_else(|| self.deferred.remove(fingerprint))?;
        let nick = entry.nick.clone();
        self.peer_map.insert(entry.peer_id, fingerprint.to_string());
        self.trusted.insert(fingerprint.to_string(), entry.key);
        Some(nick)
    }

    pub fn approve_all(&mut self) -> usize {
        let fps: Vec<String> = self.pending.keys()
            .chain(self.deferred.keys())
            .cloned()
            .collect();
        let count = fps.len();
        for fp in fps { self.approve(&fp); }
        count
    }

    pub fn reject(&mut self, fingerprint: &str) {
        self.pending.remove(fingerprint);
        self.deferred.remove(fingerprint);
        self.rejected.insert(fingerprint.to_string());
    }

    pub fn promote_deferred_to_pending(&mut self) -> usize {
        let entries: Vec<(String, PendingEntry)> = self.deferred.drain().collect();
        let count = entries.len();
        for (fp, entry) in entries { self.pending.insert(fp, entry); }
        count
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    pub fn is_known(&self, fp: &str) -> bool {
        self.trusted.contains_key(fp)
            || self.pending.contains_key(fp)
            || self.deferred.contains_key(fp)
            || self.rejected.contains(fp)
    }

    pub fn is_rejected(&self, fp: &str) -> bool {
        self.rejected.contains(fp)
    }

    pub fn trust_state(&self, fp: &str) -> Option<TrustState> {
        if self.trusted.contains_key(fp)   { return Some(TrustState::Trusted);  }
        if self.pending.contains_key(fp)   { return Some(TrustState::Pending);  }
        if self.deferred.contains_key(fp)  { return Some(TrustState::Deferred); }
        if self.rejected.contains(fp)      { return Some(TrustState::Rejected); }
        None
    }

    pub fn get_by_fingerprint(&self, fp: &str) -> Option<&SignedPublicKey> {
        self.trusted.get(fp)
    }

    pub fn get_by_peer(&self, peer_id: &PeerId) -> Option<&SignedPublicKey> {
        self.peer_map
            .get(peer_id)
            .and_then(|fp| self.trusted.get(fp))
    }

    pub fn fingerprint_for_peer(&self, peer_id: &PeerId) -> Option<&str> {
        self.peer_map.get(peer_id).map(String::as_str)
    }

    pub fn all_public_keys(&self) -> Vec<&SignedPublicKey> {
        self.trusted.values().collect()
    }

    pub fn known_fingerprints(&self) -> Vec<String> {
        self.trusted.keys().cloned().collect()
    }

    pub fn pending_keys(&self) -> Vec<(String, String)> {
        self.pending
            .iter()
            .map(|(fp, e)| (fp.clone(), e.nick.clone()))
            .collect()
    }

    pub fn deferred_keys(&self) -> Vec<(String, String)> {
        self.deferred
            .iter()
            .map(|(fp, e)| (fp.clone(), e.nick.clone()))
            .collect()
    }

    pub fn len(&self) -> usize { self.trusted.len() }
    pub fn is_empty(&self) -> bool { self.trusted.is_empty() }

    // -----------------------------------------------------------------------
    // Destructive operations
    // -----------------------------------------------------------------------

    pub fn remove_peer(&mut self, peer_id: &PeerId) {
        if let Some(fp) = self.peer_map.remove(peer_id) {
            self.trusted.remove(&fp);
        }
    }

    pub fn remove_fingerprint(&mut self, fp: &str) {
        self.trusted.remove(fp);
        self.pending.remove(fp);
        self.deferred.remove(fp);
        self.peer_map.retain(|_, v| v != fp);
    }

    pub fn nuke(&mut self) {
        self.trusted.clear();
        self.pending.clear();
        self.deferred.clear();
        self.rejected.clear();
        self.peer_map.clear();
    }
}
