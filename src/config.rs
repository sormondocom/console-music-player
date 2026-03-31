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

    // --- Amazon Music easter egg ---
    /// Browser cookie string for amazon.com / music.amazon.com.
    /// Copy from DevTools → Application → Cookies → music.amazon.com.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amazon_cookie: Option<String>,
    /// Directory where Amazon MP3 downloads are saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amazon_download_dir: Option<PathBuf>,
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
