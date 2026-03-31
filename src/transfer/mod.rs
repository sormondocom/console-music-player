//! Transfer engine: coordinates copying tracks from the local library
//! to a connected device.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::{error, info};

use crate::device::MusicDevice;
use crate::library::Track;
use crate::media::MediaItem;

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum TransferEvent {
    /// Transfer of a single track started.
    Started { track_title: String, index: usize, total: usize },
    /// A chunk of bytes was written.
    Progress { bytes_done: u64, bytes_total: u64 },
    /// One track finished successfully.
    TrackDone { track_title: String, destination: String },
    /// One track failed.
    TrackFailed { track_title: String, reason: String },
    /// All tracks in the batch are done.
    BatchComplete { succeeded: usize, failed: usize },
}

// ---------------------------------------------------------------------------
// Transfer engine
// ---------------------------------------------------------------------------

/// Manages a queue of tracks to be uploaded to a device.
pub struct TransferEngine {
    events_tx: mpsc::UnboundedSender<TransferEvent>,
    pub events_rx: Arc<Mutex<mpsc::UnboundedReceiver<TransferEvent>>>,
}

impl TransferEngine {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            events_tx: tx,
            events_rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Spawn an async task that uploads `tracks` to `device`.
    ///
    /// Progress events are sent on the internal channel and can be
    /// consumed by the UI via `poll_event`.
    pub fn start_batch(
        &self,
        device: Arc<dyn MusicDevice>,
        tracks: Vec<Track>,
    ) {
        let tx = self.events_tx.clone();

        tokio::spawn(async move {
            let total = tracks.len();
            let mut succeeded = 0usize;
            let mut failed = 0usize;

            for (index, track) in tracks.iter().enumerate() {
                let _ = tx.send(TransferEvent::Started {
                    track_title: track.display_title().to_string(),
                    index,
                    total,
                });

                info!("Transferring {}/{}: {}", index + 1, total, track.display_title());

                match device.upload_track(track) {
                    Ok(outcome) => {
                        succeeded += 1;
                        // Always emit the full step log
                        for line in &outcome.log {
                            let _ = tx.send(TransferEvent::TrackFailed {
                                track_title: String::new(),
                                reason: line.clone(),
                            });
                        }
                        if !outcome.db_updated {
                            let _ = tx.send(TransferEvent::TrackFailed {
                                track_title: outcome.track_title.clone(),
                                reason: "  ✗ file copied but iTunesDB not updated — \
                                         track won't appear in Songs.".into(),
                            });
                        }
                        let _ = tx.send(TransferEvent::TrackDone {
                            track_title: outcome.track_title,
                            destination: outcome.destination,
                        });
                    }
                    Err(e) => {
                        failed += 1;
                        error!("Transfer failed for '{}': {e}", track.display_title());
                        let _ = tx.send(TransferEvent::TrackFailed {
                            track_title: track.display_title().to_string(),
                            reason: e.to_string(),
                        });
                    }
                }
            }

            let _ = tx.send(TransferEvent::BatchComplete { succeeded, failed });
        });
    }

    /// Non-blocking poll for the next event from the transfer task.
    pub fn poll_event(&self) -> Option<TransferEvent> {
        self.events_rx
            .lock()
            .ok()?
            .try_recv()
            .ok()
    }
}

impl Default for TransferEngine {
    fn default() -> Self {
        Self::new()
    }
}
