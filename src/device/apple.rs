//! Apple device support via the `idevice` crate (pure Rust, no C deps).
//!
//! ## How iOS music transfer works
//!
//! Files are copied to the device via the **Apple File Conduit (AFC)**
//! service, which is started through `lockdownd`. The media directory
//! on iOS is `/iTunes_Control/Music/`. Each track is given a unique
//! filename (e.g. `MUSI0001.mp3`). The iTunes media library database
//! (`/iTunes_Control/iTunes/MediaLibrary.sqlitedb`) must then be
//! updated for the track to appear in the Music app — that second step
//! is deferred to a later iteration; for now we copy the raw file.
//!
//! ## Platform requirements
//!
//! | Platform | Requirement |
//! |----------|-------------|
//! | Windows  | iTunes (provides the Apple Mobile Device USB Driver + usbmuxd) |
//! | Linux    | `usbmuxd` package installed and running |
//! | macOS    | Built-in support via the OS usbmuxd socket |

use std::fmt;
use std::fs;
use std::path::Path;

use tracing::{debug, info, warn};

use super::{DeviceKind, MusicDevice, TransferOutcome};
use crate::error::{AppError, Result};
use crate::library::Track;
use crate::media::MediaItem;

// ---------------------------------------------------------------------------
// Apple device representation
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AppleDevice {
    pub udid: String,
    pub name: String,
    pub kind: DeviceKind,
    /// Cached free space, fetched at enumeration time.
    pub free_bytes: Option<u64>,
}

impl fmt::Display for AppleDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}) [{}]", self.name, self.kind, self.udid)
    }
}

impl MusicDevice for AppleDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> &DeviceKind {
        &self.kind
    }

    fn udid(&self) -> &str {
        &self.udid
    }

    fn free_space(&self) -> Option<u64> {
        self.free_bytes
    }

    fn upload_track(&self, track: &Track) -> Result<TransferOutcome> {
        upload_track_to_device(&self.udid, track)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Device enumeration
// ---------------------------------------------------------------------------

/// Return all Apple devices currently connected via USB.
///
/// This calls into `idevice` to query the local usbmuxd socket.
/// If usbmuxd is not running or no devices are connected, returns
/// an empty Vec (not an error).
pub fn enumerate_apple_devices() -> Result<Vec<Box<dyn MusicDevice>>> {
    // NOTE: idevice 0.1.x API is unstable. The call below follows the
    // pattern shown in the crate's README / examples. Pin Cargo.toml
    // to "=0.1.53" to guard against breaking changes until 0.2.0.
    //
    // idevice::usbmuxd::UsbmuxdConnection::connect() returns a future;
    // we run it synchronously here via tokio::task::block_in_place so
    // this function remains non-async for callers that need a simple Vec.

    let devices = match query_usbmuxd_devices() {
        Ok(d) => d,
        Err(e) => {
            warn!("Could not query usbmuxd (is a device connected?): {e}");
            return Ok(Vec::new());
        }
    };

    Ok(devices)
}

/// Internal: ask usbmuxd for the list of paired devices.
fn query_usbmuxd_devices() -> Result<Vec<Box<dyn MusicDevice>>> {
    // TODO(idevice-api): Replace the stub below with real idevice calls
    // once we confirm the exact 0.1.53 surface area.  The shape will be
    // something like:
    //
    //   let conn = idevice::usbmuxd::UsbmuxdConnection::connect().await?;
    //   let device_list = conn.list_devices().await?;
    //   for dev in device_list {
    //       let lockdown = idevice::lockdownd::LockdowndClient::connect(&dev).await?;
    //       let name = lockdown.get_value("DeviceName", None).await?;
    //       let product = lockdown.get_value("ProductType", None).await?;
    //       ...
    //   }
    //
    // For now we return an empty list so the rest of the app compiles
    // and runs without a connected device.
    debug!("query_usbmuxd_devices: idevice integration pending");
    Ok(Vec::new())
}

// ---------------------------------------------------------------------------
// File transfer
// ---------------------------------------------------------------------------

/// Copy a single audio file to the device's media directory via AFC.
///
/// The file is placed under `/iTunes_Control/Music/` using the same
/// base filename.  A future iteration will generate a unique name
/// and update the media library database.
fn upload_track_to_device(udid: &str, track: &Track) -> Result<TransferOutcome> {
    info!(
        "Uploading '{}' to device {}",
        track.display_title(),
        udid
    );

    let file_name = track
        .path
        .file_name()
        .ok_or_else(|| AppError::Transfer("Track path has no filename".into()))?
        .to_string_lossy()
        .to_string();

    let destination = format!("/iTunes_Control/Music/{file_name}");

    // TODO(idevice-api): Replace with real AFC transfer.
    //
    //   let conn  = idevice::usbmuxd::UsbmuxdConnection::connect().await?;
    //   let dev   = conn.get_device(udid).await?;
    //   let lock  = idevice::lockdownd::LockdowndClient::connect(&dev).await?;
    //   let svc   = lock.start_service("com.apple.afc").await?;
    //   let afc   = idevice::afc::AfcClient::connect(svc).await?;
    //
    //   afc.make_directory("/iTunes_Control/Music").await.ok(); // ignore if exists
    //
    //   let data  = std::fs::read(&track.path)?;
    //   afc.file_write(&destination, &data).await?;
    //
    afc_transfer_file(&track.path, &destination)?;

    Ok(TransferOutcome {
        track_title: track.display_title().to_string(),
        bytes_copied: track.file_size,
        destination,
        db_updated: false,
        log: vec!["  (AFC transfer — DB update not yet implemented)".into()],
    })
}

/// Low-level AFC file write. Currently a stub.
fn afc_transfer_file(local: &Path, _remote: &str) -> Result<()> {
    // Verify the local file is readable before attempting anything.
    let _bytes = fs::read(local).map_err(|e| {
        AppError::Transfer(format!("Cannot read {}: {e}", local.display()))
    })?;

    // TODO(idevice-api): stream `_bytes` to `_remote` on the device via AFC.
    // Implement once idevice AFC bindings are confirmed.
    warn!("afc_transfer_file: AFC transfer not yet implemented — file read OK, remote write pending");
    Err(AppError::Transfer(
        "AFC transfer not yet implemented — see src/device/apple.rs TODO".into(),
    ))
}
