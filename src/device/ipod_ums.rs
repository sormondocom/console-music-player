//! USB Mass Storage iPod support — thin wrapper around the `ipod-rs` library.

use ipod_rs::{DeviceScanResult, DeviceTrackEntry, IpodDevice, IpodTrack, IncompleteEntry, OrphanedFile, UploadResult};

use super::{DeviceKind, MusicDevice, TransferOutcome};
use crate::error::{AppError, Result};
use crate::library::Track;
use crate::media::MediaItem;

// ---------------------------------------------------------------------------
// Device struct
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct IpodUmsDevice {
    pub inner: IpodDevice,
    display: String,
    udid_str: String,
}

impl IpodUmsDevice {
    /// Human-readable firmware / generation string (e.g. "4th generation (DB v9)").
    pub fn firmware_label(&self) -> String {
        self.inner.firmware.to_string()
    }

    /// Scan the device for orphaned files and incomplete DB entries.
    pub fn scan_health(&self) -> Result<DeviceScanResult> {
        self.inner
            .scan_health()
            .map_err(|e| AppError::Transfer(e.to_string()))
    }

    /// Repair a track whose mhit exists but is missing from the master playlist.
    pub fn repair_incomplete(&self, entry: &IncompleteEntry) -> Result<()> {
        self.inner
            .repair_incomplete(entry)
            .map_err(|e| AppError::Transfer(e.to_string()))
    }

    /// Register an orphaned file into the database.
    pub fn repair_orphan(&self, orphan: &OrphanedFile) -> Result<()> {
        self.inner
            .repair_orphan(orphan)
            .map_err(|e| AppError::Transfer(e.to_string()))
    }

    /// List all tracks currently on the device.
    ///
    /// Reads from iTunesDB when available; falls back to filesystem scan.
    pub fn list_tracks(&self) -> Vec<DeviceTrackEntry> {
        self.inner.list_tracks()
    }

    /// Run the DB location search and return a diagnostic log of every path
    /// checked. Call when `list_tracks` falls back to filesystem scan.
    pub fn diagnose_db_location(&self) -> Vec<String> {
        self.inner.diagnose_db_location()
    }

    /// Create a fresh iTunesDB on the device if one doesn't already exist.
    pub fn init_database(&self) -> Result<()> {
        self.inner
            .init_database()
            .map(|_| ())
            .map_err(|e| AppError::Transfer(e.to_string()))
    }
}

impl MusicDevice for IpodUmsDevice {
    fn name(&self) -> &str {
        &self.display
    }

    fn kind(&self) -> &DeviceKind {
        &DeviceKind::IPod
    }

    fn udid(&self) -> &str {
        &self.udid_str
    }

    fn free_space(&self) -> Option<u64> {
        self.inner.free_space()
    }

    fn firmware_label(&self) -> String {
        self.inner.firmware.to_string()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn upload_track(&self, track: &Track) -> Result<TransferOutcome> {
        let ipod_track = track_to_ipod(track);
        let UploadResult { ipod_rel_path, db_updated, log } = self
            .inner
            .upload(&ipod_track)
            .map_err(|e| AppError::Transfer(e.to_string()))?;

        let bytes = track.path.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(TransferOutcome {
            track_title: track.display_title().to_string(),
            bytes_copied: bytes,
            destination: ipod_rel_path,
            db_updated,
            log,
        })
    }
}

// ---------------------------------------------------------------------------
// Device enumeration
// ---------------------------------------------------------------------------

pub fn enumerate_ipod_ums_devices() -> Result<Vec<Box<dyn MusicDevice>>> {
    let devices = IpodDevice::detect()
        .into_iter()
        .map(|inner| {
            let display = inner.display_name();
            let udid_str = format!("ums:{}", inner.root.display());
            Box::new(IpodUmsDevice { inner, display, udid_str }) as Box<dyn MusicDevice>
        })
        .collect();
    Ok(devices)
}

// ---------------------------------------------------------------------------
// Conversion helper
// ---------------------------------------------------------------------------

fn track_to_ipod(track: &Track) -> IpodTrack {
    let title = if track.title.is_empty() {
        track
            .path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string()
    } else {
        track.title.clone()
    };

    IpodTrack {
        local_path: track.path.clone(),
        title,
        artist: track.artist.clone(),
        album: track.album.clone(),
        duration_ms: track.duration_secs.unwrap_or(0).saturating_mul(1000),
        file_size: track.file_size,
        bitrate_kbps: track.bitrate_kbps.unwrap_or(0),
        sample_rate_hz: track.sample_rate_hz.unwrap_or(0),
        year: 0,
    }
}
