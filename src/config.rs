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

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// Directories scanned for music files.
    pub source_dirs: Vec<PathBuf>,

    // ── P2P identity (all optional — absent means P2P never activated) ──────
    /// ASCII-armoured secret key (protected by `p2p_identity_passphrase`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_identity_armored: Option<String>,

    /// Auto-generated passphrase protecting the secret key.
    /// Beta simplification — TODO: move to OS keychain before stable.
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
