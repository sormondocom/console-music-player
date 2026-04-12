//! In-memory track transfer: inbound buffer assembly.
//!
//! Chunks arrive over gossipsub and are accumulated here until all are present.
//! On completion the buffer is integrity-checked via SHA-256 and emitted as
//! `P2pEvent::TrackBufferReady` for immediate in-memory playback.
//!
//! Note: chunks are currently sent as plaintext over Noise-encrypted transport.
//! TODO (before stable): ECDH-encrypt each chunk to the requester's subkey so
//! that gossipsub broadcast doesn't expose audio data to non-requesting peers.

use std::collections::HashMap;

use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::p2p::wire::RemoteTrack;

/// Maximum plaintext chunk size per gossipsub message (64 KiB).
/// Gossipsub supports up to 1 MiB per message; 64 KiB leaves ample headroom
/// for the signing envelope and JSON overhead.
pub const CHUNK_SIZE: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// InboundTransfer — per-transfer receive state
// ---------------------------------------------------------------------------

/// Maximum number of `ChunkNack` retries before the transfer is abandoned.
pub const MAX_CHUNK_RETRIES: u32 = 3;

/// State for an inbound track transfer being assembled in RAM.
///
/// Chunks are stored by index until all arrive, then assembled and
/// integrity-checked before emitting `P2pEvent::TrackBufferReady`.
pub struct InboundTransfer {
    pub track: RemoteTrack,
    pub total_chunks: u32,
    /// Received plaintext chunks, keyed by 0-based index.
    chunks: HashMap<u32, Vec<u8>>,
    /// Expected SHA-256 hex digest; set when `TrackComplete` arrives.
    pub expected_hash: Option<String>,
    /// How many `ChunkNack` retries have been sent for this transfer.
    pub retry_count: u32,
}

impl InboundTransfer {
    pub fn new(track: RemoteTrack, total_chunks: u32) -> Self {
        Self {
            track,
            total_chunks,
            chunks: HashMap::new(),
            expected_hash: None,
            retry_count: 0,
        }
    }

    /// Store a received chunk (silently ignores duplicate indices).
    pub fn insert_chunk(&mut self, index: u32, data: Vec<u8>) {
        self.chunks.entry(index).or_insert(data);
    }

    /// Total plaintext bytes received so far.
    pub fn received_bytes(&self) -> u64 {
        self.chunks.values().map(|c| c.len() as u64).sum()
    }

    /// `true` when every expected chunk has arrived.
    pub fn is_complete(&self) -> bool {
        self.chunks.len() as u32 == self.total_chunks
    }

    /// Returns indices of chunks that have not yet been received.
    pub fn missing_indices(&self) -> Vec<u32> {
        (0..self.total_chunks)
            .filter(|i| !self.chunks.contains_key(i))
            .collect()
    }

    /// Assemble chunks in index order and verify SHA-256 integrity.
    ///
    /// Returns `Err(reason)` if any chunk is missing or the hash mismatches.
    /// The assembled bytes are wrapped in `Zeroizing` so they wipe themselves
    /// from memory when the `Vec` is dropped after decoding.
    pub fn assemble(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        let cap = self.track.file_size as usize;
        let mut buf = Zeroizing::new(Vec::with_capacity(cap));
        for i in 0..self.total_chunks {
            let chunk = self
                .chunks
                .get(&i)
                .ok_or_else(|| format!("missing chunk {i}/{}", self.total_chunks))?;
            buf.extend_from_slice(chunk);
        }
        if let Some(expected) = &self.expected_hash {
            let actual = sha256_hex(buf.as_slice());
            if actual != *expected {
                return Err(format!(
                    "integrity check failed — expected {expected}, got {actual}"
                ));
            }
        }
        Ok(buf)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute SHA-256 of `bytes` and return a lowercase hex string.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
