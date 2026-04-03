//! Platform-specific detection for local Amazon Music installations.
//!
//! Returns information about the Amazon Music desktop app and its local
//! download directory so we can import already-downloaded purchases directly
//! without touching the web API.

use std::path::PathBuf;

/// What we know about the local Amazon Music installation.
#[derive(Debug, Clone)]
pub struct AmazonMusicLocal {
    /// Path to the Amazon Music executable, if found (Win32 installs only).
    pub exe: Option<PathBuf>,
    /// True when the app is installed as a Windows Store (UWP) app.
    pub is_uwp: bool,
    /// Directory where Amazon Music stores downloaded MP3s.
    pub download_dir: PathBuf,
    /// True when the download_dir actually exists on disk.
    pub download_dir_exists: bool,
}

impl AmazonMusicLocal {
    /// True if the app is installed in any form (Win32 exe or UWP Store app).
    pub fn is_installed(&self) -> bool {
        self.exe.is_some() || self.is_uwp
    }

    /// Human-readable description of what was found.
    pub fn summary(&self) -> String {
        let app_label = if self.is_uwp {
            "Amazon Music (Store app)".to_string()
        } else if let Some(exe) = &self.exe {
            format!("Amazon Music app: {}", exe.display())
        } else {
            "Amazon Music not detected".to_string()
        };

        if self.download_dir_exists {
            format!("{}  |  Downloads: {}", app_label, self.download_dir.display())
        } else {
            format!("{} (download dir not found at {})", app_label, self.download_dir.display())
        }
    }
}

/// Detect the local Amazon Music installation for the current platform.
/// Always returns a value — check `exe` and `download_dir_exists` to know
/// what was actually found.
pub fn detect_amazon_music() -> AmazonMusicLocal {
    #[cfg(target_os = "windows")]
    return detect_windows();

    #[cfg(target_os = "android")]
    return detect_android();

    #[cfg(not(any(target_os = "windows", target_os = "android")))]
    return detect_unix();
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn detect_windows() -> AmazonMusicLocal {
    let local_appdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
    let userprofile = std::env::var("USERPROFILE").unwrap_or_default();

    // Candidate exe locations for the classic Win32 installer.
    let exe = find_first_existing(&[
        r"C:\Program Files\Amazon Music\Amazon Music.exe",
        r"C:\Program Files (x86)\Amazon Music\Amazon Music.exe",
        &format!(r"{local_appdata}\Programs\Amazon Music\Amazon Music.exe"),
        &format!(r"{local_appdata}\Amazon Music\Amazon Music.exe"),
    ]);

    // Check for the Windows Store (UWP) version via its package data directory.
    // The family name is stable across version updates; the versioned path changes.
    const UWP_FAMILY: &str = "AmazonMobileLLC.AmazonMusic_kc6t79cpj4tp0";
    let is_uwp = !local_appdata.is_empty()
        && PathBuf::from(&local_appdata).join("Packages").join(UWP_FAMILY).is_dir();

    // Download directory.
    // Win32 install: %USERPROFILE%\Music\Amazon Music
    // UWP install:   internal app storage — downloads are not accessible as plain files,
    //                so we fall back to the Win32 default path (may not exist).
    let download_dir = PathBuf::from(&userprofile).join("Music").join("Amazon Music");
    let download_dir = if download_dir.is_dir() {
        download_dir
    } else if !userprofile.is_empty() {
        download_dir
    } else {
        PathBuf::from(r"C:\Users\Default\Music\Amazon Music")
    };

    let download_dir_exists = download_dir.is_dir();
    AmazonMusicLocal { exe, is_uwp, download_dir, download_dir_exists }
}

// ---------------------------------------------------------------------------
// Android
// ---------------------------------------------------------------------------

#[cfg(target_os = "android")]
fn detect_android() -> AmazonMusicLocal {
    // Amazon Music on Android stores downloads under Music/Amazon Music.
    let candidates = [
        "/storage/emulated/0/Music/Amazon Music",
        "/sdcard/Music/Amazon Music",
        "/storage/emulated/0/Amazon Music",
    ];
    let download_dir = candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("/storage/emulated/0/Music/Amazon Music"));

    let download_dir_exists = download_dir.is_dir();
    AmazonMusicLocal { exe: None, is_uwp: false, download_dir, download_dir_exists }
}

// ---------------------------------------------------------------------------
// Linux / macOS fallback
// ---------------------------------------------------------------------------

#[cfg(not(any(target_os = "windows", target_os = "android")))]
fn detect_unix() -> AmazonMusicLocal {
    let home = std::env::var("HOME").unwrap_or_default();
    let download_dir = PathBuf::from(&home).join("Music").join("Amazon Music");
    let download_dir_exists = download_dir.is_dir();
    AmazonMusicLocal { exe: None, is_uwp: false, download_dir, download_dir_exists }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn find_first_existing(paths: &[&str]) -> Option<PathBuf> {
    paths.iter().map(PathBuf::from).find(|p| p.exists())
}

/// Launch the Amazon Music desktop app (fire-and-forget).
/// Returns `true` if the process was started successfully.
pub fn launch_amazon_music(local: &AmazonMusicLocal) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        if local.is_uwp {
            // UWP apps can't be launched via their exe directly (restricted path).
            // Use the shell:AppsFolder protocol instead.
            const APP_ID: &str =
                "shell:AppsFolder\\AmazonMobileLLC.AmazonMusic_kc6t79cpj4tp0!AmazonMobileLLC.AmazonMusic";
            return std::process::Command::new("explorer.exe")
                .arg(APP_ID)
                .creation_flags(0x00000008)
                .spawn()
                .is_ok();
        }

        let exe = match &local.exe {
            Some(p) => p,
            None => return false,
        };
        std::process::Command::new(exe)
            .creation_flags(0x00000008) // DETACHED_PROCESS
            .spawn()
            .is_ok()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let exe = match &local.exe {
            Some(p) => p,
            None => return false,
        };
        std::process::Command::new(exe).spawn().is_ok()
    }
}
