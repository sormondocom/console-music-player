//! Small cross-cutting utilities.

use std::path::PathBuf;

/// Expand a leading `~` to the current user's home directory.
///
/// Handles these forms:
///   `~`          → home dir
///   `~/foo/bar`  → home dir + /foo/bar
///   `/abs/path`  → returned unchanged
///   `rel/path`   → returned unchanged
pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(s)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}
