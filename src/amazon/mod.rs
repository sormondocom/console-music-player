//! Amazon Music — local installation support.
//!
//! The web API approach was abandoned. The app now detects the Amazon Music
//! desktop application and its local download directory via `crate::platform`.

pub fn push<T>(_: &std::sync::Arc<std::sync::Mutex<Vec<T>>>, _: T) {}

pub fn local_asin_set_from_filenames(_: &[std::path::PathBuf]) -> std::collections::HashSet<String> {
    std::collections::HashSet::new()
}
