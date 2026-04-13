//! Persistent application configuration.
//!
//! Stored as JSON at:
//!   Windows : %APPDATA%\console-music-player\config.json
//!   Linux   : $HOME/.config/console-music-player/config.json
//!   macOS   : $HOME/.config/console-music-player/config.json

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Directories scanned for music files.
    pub source_dirs: Vec<PathBuf>,

    // ── P2P identity (all optional — absent means P2P never activated) ──────
    /// ASCII-armoured secret key (protected by `p2p_identity_passphrase`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_identity_armored: Option<String>,

    /// Passphrase protecting the secret key.
    ///
    /// **Deprecated storage path.** On platforms with a native credential store
    /// (Windows Credential Manager, macOS Keychain, Linux Secret Service) this
    /// field is `None` — the passphrase lives in the OS keychain instead.
    ///
    /// This field is kept for two reasons:
    ///   1. **Migration** — existing installs that wrote the passphrase here
    ///      before the keychain integration can still load their identity.
    ///      `activate_p2p()` moves the value to the keychain on first run and
    ///      clears this field.
    ///   2. **Fallback** — Android/Termux and headless Linux (no secret-service
    ///      daemon) continue to use this field because no keychain is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_identity_passphrase: Option<String>,

    /// Display name for this node on the P2P network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_nickname: Option<String>,

    /// Peers explicitly trusted across sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p2p_trusted_peers: Vec<TrustedPeerRecord>,

    /// Bootstrap multiaddrs (user-configurable).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub p2p_bootstrap_peers: Vec<String>,

    /// Fixed TCP/UDP listen port for P2P (enables consistent port-forwarding).
    /// If absent or 0, a random port is assigned each session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_listen_port: Option<u16>,

    // ── Tunable settings ─────────────────────────────────────────────────────
    /// How many times a receiver retries missing chunks before giving up.
    /// Default: 5.
    #[serde(default = "default_chunk_retries")]
    pub p2p_chunk_retries: u32,

    /// LAN beacon broadcast interval in seconds.  Lower = faster discovery,
    /// higher = less broadcast noise.  Default: 2.
    #[serde(default = "default_beacon_interval_secs")]
    pub p2p_beacon_interval_secs: u64,

    /// mDNS probe interval in seconds.  Default: 30.
    #[serde(default = "default_mdns_interval_secs")]
    pub p2p_mdns_interval_secs: u64,

    /// Seconds of inactivity before a stalled chunk transfer is flagged.
    /// Default: 5.
    #[serde(default = "default_stall_secs")]
    pub p2p_stall_secs: u64,

    /// Seconds after stall before a transfer is abandoned entirely.
    /// Default: 30.
    #[serde(default = "default_abandon_secs")]
    pub p2p_abandon_secs: u64,
}

fn default_chunk_retries()      -> u32 { 5 }
fn default_beacon_interval_secs() -> u64 { 2 }
fn default_mdns_interval_secs() -> u64 { 30 }
fn default_stall_secs()         -> u64 { 5 }
fn default_abandon_secs()       -> u64 { 30 }

impl Default for Config {
    fn default() -> Self {
        Self {
            source_dirs:              Vec::new(),
            p2p_identity_armored:     None,
            p2p_identity_passphrase:  None,
            p2p_nickname:             None,
            p2p_trusted_peers:        Vec::new(),
            p2p_bootstrap_peers:      Vec::new(),
            p2p_listen_port:          None,
            p2p_chunk_retries:        default_chunk_retries(),
            p2p_beacon_interval_secs: default_beacon_interval_secs(),
            p2p_mdns_interval_secs:   default_mdns_interval_secs(),
            p2p_stall_secs:           default_stall_secs(),
            p2p_abandon_secs:         default_abandon_secs(),
        }
    }
}

/// A trusted peer persisted across sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPeerRecord {
    pub fingerprint:        String,
    pub nickname:           String,
    pub public_key_armored: String,
}

impl Config {
    /// Load config from disk. Returns a default config on any error.
    pub fn load() -> Self {
        let path = match config_path() {
            Some(p) => p,
            None => {
                warn!("Could not determine config path; using defaults.");
                return Self::default();
            }
        };

        match fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                warn!("Config parse error ({}): {e} — using defaults.", path.display());
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!("No config file found at {} — starting fresh.", path.display());
                Self::default()
            }
            Err(e) => {
                warn!("Could not read config ({}): {e} — using defaults.", path.display());
                Self::default()
            }
        }
    }

    /// Save config to disk. Errors are logged but not propagated.
    pub fn save(&self) {
        let path = match config_path() {
            Some(p) => p,
            None => {
                warn!("Could not determine config path; config not saved.");
                return;
            }
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                warn!("Could not create config dir {}: {e}", parent.display());
                return;
            }
        }

        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    warn!("Could not write config to {}: {e}", path.display());
                } else {
                    info!("Config saved to {}", path.display());
                }
            }
            Err(e) => warn!("Could not serialize config: {e}"),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var("APPDATA").ok().map(PathBuf::from)?
    } else {
        let home = std::env::var("HOME").ok().map(PathBuf::from)?;
        home.join(".config")
    };
    Some(base.join("console-music-player").join("config.json"))
}
