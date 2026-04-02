//! File organizer engine: copy → verify → delete, with progress reporting.
//!
//! Follows the same async-channel pattern as `transfer/mod.rs`.
//!
//! Verification: after `std::fs::copy` succeeds, the destination file size is
//! compared to the source file size.  A mismatch causes the copy to be
//! retracted (destination deleted) and the track is reported as failed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tracing::info;

use crate::library::Track;
use crate::media::MediaItem;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum OrganizerEvent {
    /// Copy of a single track started.
    Started { title: String, index: usize, total: usize },
    /// One track copied, verified, and original deleted.
    FileDone { title: String, dest: String },
    /// One track failed (copy, verify, or delete step).
    FileFailed { title: String, reason: String },
    /// All tracks done.  `dest_dir` is added as a new source by the caller.
    BatchComplete { succeeded: usize, failed: usize, dest_dir: PathBuf },
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

pub struct OrganizerEngine {
    events_tx: mpsc::UnboundedSender<OrganizerEvent>,
    pub events_rx: Arc<Mutex<mpsc::UnboundedReceiver<OrganizerEvent>>>,
}

impl OrganizerEngine {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            events_tx: tx,
            events_rx: Arc::new(Mutex::new(rx)),
        }
    }

    /// Non-blocking poll for the next event.
    pub fn poll_event(&self) -> Option<OrganizerEvent> {
        self.events_rx.lock().ok()?.try_recv().ok()
    }

    /// Spawn an async task that copies `tracks` to `dest_dir`.
    /// If `dest_dir` doesn't exist it is created.  After each successful copy
    /// the source file is deleted.  The original `dest_dir` is forwarded in
    /// `BatchComplete` so the caller can add it as a new source.
    pub fn start_batch(&self, tracks: Vec<Track>, dest_dir: PathBuf) {
        let tx = self.events_tx.clone();
        tokio::spawn(async move {
            let total = tracks.len();
            let mut succeeded = 0usize;
            let mut failed = 0usize;

            if let Err(e) = std::fs::create_dir_all(&dest_dir) {
                let _ = tx.send(OrganizerEvent::FileFailed {
                    title: String::new(),
                    reason: format!("Cannot create destination '{}': {e}", dest_dir.display()),
                });
                let _ = tx.send(OrganizerEvent::BatchComplete { succeeded, failed: total, dest_dir });
                return;
            }

            for (index, track) in tracks.iter().enumerate() {
                let title = track.display_title().to_string();
                let _ = tx.send(OrganizerEvent::Started {
                    title: title.clone(),
                    index,
                    total,
                });

                let file_name = match track.path.file_name() {
                    Some(n) => n,
                    None => {
                        failed += 1;
                        let _ = tx.send(OrganizerEvent::FileFailed {
                            title,
                            reason: "Cannot determine file name".into(),
                        });
                        continue;
                    }
                };

                let dest_path = dest_dir.join(file_name);

                // If source and dest are the same path, skip silently.
                if dest_path == track.path {
                    succeeded += 1;
                    let _ = tx.send(OrganizerEvent::FileDone {
                        title,
                        dest: dest_path.display().to_string(),
                    });
                    continue;
                }

                match copy_and_verify(&track.path, &dest_path) {
                    Ok(()) => {
                        info!("Organized [{}/{}]: {} → {}",
                            index + 1, total, track.path.display(), dest_path.display());
                        match std::fs::remove_file(&track.path) {
                            Ok(()) => {
                                succeeded += 1;
                                let _ = tx.send(OrganizerEvent::FileDone {
                                    title,
                                    dest: dest_path.display().to_string(),
                                });
                            }
                            Err(e) => {
                                // Copy succeeded but delete failed — keep the copy,
                                // report a warning rather than a hard failure.
                                succeeded += 1;
                                let _ = tx.send(OrganizerEvent::FileDone {
                                    title: title.clone(),
                                    dest: dest_path.display().to_string(),
                                });
                                let _ = tx.send(OrganizerEvent::FileFailed {
                                    title,
                                    reason: format!("  ⚠ copied but could not delete original: {e}"),
                                });
                            }
                        }
                    }
                    Err(e) => {
                        failed += 1;
                        let _ = tx.send(OrganizerEvent::FileFailed { title, reason: e });
                    }
                }
            }

            let _ = tx.send(OrganizerEvent::BatchComplete { succeeded, failed, dest_dir });
        });
    }
}

impl Default for OrganizerEngine {
    fn default() -> Self { Self::new() }
}

// ---------------------------------------------------------------------------
// copy + verify
// ---------------------------------------------------------------------------

/// Copy `src` to `dest`, then verify that destination file size matches
/// source file size.  If verification fails, the destination is removed and
/// an `Err` is returned so the source is not deleted.
fn copy_and_verify(src: &Path, dest: &Path) -> Result<(), String> {
    let src_size = std::fs::metadata(src)
        .map_err(|e| format!("Cannot stat source: {e}"))?
        .len();

    std::fs::copy(src, dest)
        .map_err(|e| format!("Copy failed: {e}"))?;

    let dest_size = std::fs::metadata(dest)
        .map_err(|e| format!("Cannot stat destination after copy: {e}"))?
        .len();

    if dest_size != src_size {
        let _ = std::fs::remove_file(dest);
        return Err(format!(
            "Verification failed: source {src_size} bytes, destination {dest_size} bytes"
        ));
    }

    Ok(())
}
