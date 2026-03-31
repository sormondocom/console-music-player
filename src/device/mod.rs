pub mod apple;
pub mod ipod_ums;

use std::any::Any;
use std::fmt;

use crate::error::Result;
use crate::library::Track;

/// Describes the type of connected device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceKind {
    IPhone,
    IPod,
    /// Catch-all for other Apple devices (iPad, etc.)
    AppleOther(String),
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceKind::IPhone => write!(f, "iPhone"),
            DeviceKind::IPod => write!(f, "iPod"),
            DeviceKind::AppleOther(s) => write!(f, "{s}"),
        }
    }
}

/// Result of a single-track transfer operation.
#[derive(Debug)]
pub struct TransferOutcome {
    pub track_title: String,
    pub bytes_copied: u64,
    pub destination: String,
    /// Whether the device database was updated. If `false`, the file was
    /// copied to the device but won't appear in the device's song menus.
    pub db_updated: bool,
    /// Step-by-step log of every action taken during the upload.
    pub log: Vec<String>,
}

/// Common interface for any connected music device.
pub trait MusicDevice: Send + Sync + fmt::Debug + Any {
    /// Human-readable device name (e.g. "Shawn's iPhone").
    fn name(&self) -> &str;

    /// Device kind.
    fn kind(&self) -> &DeviceKind;

    /// Unique identifier (UDID for Apple devices).
    fn udid(&self) -> &str;

    /// Free space on the device in bytes, if available.
    fn free_space(&self) -> Option<u64>;

    /// Copy a single track onto the device media library.
    fn upload_track(&self, track: &Track) -> Result<TransferOutcome>;

    /// Human-readable firmware / generation label (empty if unknown).
    fn firmware_label(&self) -> String {
        String::new()
    }

    /// Enable downcasting to concrete device types.
    fn as_any(&self) -> &dyn Any;
}

/// Enumerate all currently connected music devices.
///
/// Searches for Apple devices via usbmuxd. Additional backends
/// (MTP, etc.) can be added here in the future.
pub fn enumerate_devices() -> Result<Vec<Box<dyn MusicDevice>>> {
    let mut devices: Vec<Box<dyn MusicDevice>> = Vec::new();
    // USB Mass Storage iPods (classic, mini, nano, shuffle)
    devices.extend(ipod_ums::enumerate_ipod_ums_devices()?);
    // Apple devices via usbmuxd/lockdownd (iPhone, iPod touch)
    devices.extend(apple::enumerate_apple_devices()?);
    Ok(devices)
}
