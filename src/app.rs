//! Top-level application state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rodio::OutputStreamHandle;

use ipod_rs::{DeviceScanResult, DeviceTrackEntry};

use crate::device::ipod_ums::IpodUmsDevice;
use crate::device::MusicDevice;
use crate::library::dedup::{self, DedupAction, DuplicateGroup};
use crate::library::scanner;
use crate::library::{Library, TrackEdit};
use crate::media::MediaItem;
use crate::player::Player;
use crate::playlist::{ConflictCtx, Playlist};
use crate::amazon::{AmazonMsg, AmazonTrack};
use crate::tags::TagStore;
use crate::transfer::{TransferEngine, TransferEvent};

// ---------------------------------------------------------------------------
// Screens / focus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Library,
    Sources,
    AddSource,
    /// Browse / load saved playlists.
    Playlists,
    /// Input box: type a name to save the current selection as a playlist.
    SavePlaylist,
    /// A playlist name collision was detected — user picks resolution.
    PlaylistConflict,
    Transfer,
    /// iPod health scan results — orphaned files and incomplete DB entries.
    RepairIpod,
    /// Browse tracks currently stored on the selected iPod.
    DeviceTracks,
    /// Multi-field metadata editor for the focused local track.
    EditTrack,
    /// Interactive duplicate-track finder and resolver.
    Dedup,
    /// Amazon Music easter egg — download owned DRM-free MP3s.
    Amazon,
}

// ---------------------------------------------------------------------------
// Edit state
// ---------------------------------------------------------------------------

/// Field order used in the tag editor overlay.
pub const EDIT_FIELD_LABELS: [&str; 5] = ["Title  ", "Artist ", "Album  ", "Year   ", "Genre  "];

/// Transient state for the tag editor screen.
#[derive(Debug, Clone)]
pub struct EditState {
    /// Path of the track being edited, used to call `library.apply_edit`.
    pub path: PathBuf,
    /// Editable text for each field in `EDIT_FIELD_LABELS` order.
    pub fields: [String; 5],
    /// Which field the cursor is on (0–4).
    pub focused_field: usize,
}

// ---------------------------------------------------------------------------
// Dedup state
// ---------------------------------------------------------------------------

/// Which panel of the dedup screen has keyboard focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupFocus {
    /// Left panel — scrolling through duplicate groups.
    Groups,
    /// Right panel — navigating candidates within the focused group.
    Candidates,
}

/// All state needed for the deduplication screen.
#[derive(Debug, Clone)]
pub struct DedupState {
    pub groups: Vec<DuplicateGroup>,
    /// Index of the focused group in the left panel.
    pub group_index: usize,
    /// Index of the focused candidate in the right panel.
    pub candidate_index: usize,
    /// Per-group, per-candidate action chosen by the user.
    pub actions: Vec<Vec<DedupAction>>,
    pub focus: DedupFocus,
}

impl DedupState {
    pub fn new(groups: Vec<DuplicateGroup>) -> Self {
        let actions = groups.iter().map(|g| g.suggested_actions()).collect();
        Self {
            groups,
            group_index: 0,
            candidate_index: 0,
            actions,
            focus: DedupFocus::Groups,
        }
    }

    pub fn focused_group(&self) -> Option<&DuplicateGroup> {
        self.groups.get(self.group_index)
    }

    pub fn focused_actions(&self) -> Option<&Vec<DedupAction>> {
        self.actions.get(self.group_index)
    }

    pub fn group_count(&self) -> usize { self.groups.len() }

    pub fn to_delete_count(&self) -> usize {
        self.actions.iter().flatten().filter(|&&a| a == DedupAction::Delete).count()
    }

    pub fn move_group_up(&mut self) {
        if self.group_index > 0 {
            self.group_index -= 1;
            self.candidate_index = 0;
        }
    }

    pub fn move_group_down(&mut self) {
        if self.group_index + 1 < self.groups.len() {
            self.group_index += 1;
            self.candidate_index = 0;
        }
    }

    pub fn move_candidate_up(&mut self) {
        if self.candidate_index > 0 {
            self.candidate_index -= 1;
        }
    }

    pub fn move_candidate_down(&mut self) {
        let max = self.focused_group().map(|g| g.candidates.len()).unwrap_or(0);
        if self.candidate_index + 1 < max {
            self.candidate_index += 1;
        }
    }

    /// Cycle the action for the focused candidate.
    pub fn toggle_focused_action(&mut self) {
        if let Some(actions) = self.actions.get_mut(self.group_index) {
            if let Some(action) = actions.get_mut(self.candidate_index) {
                *action = action.cycle();
            }
        }
    }

    /// Re-apply auto-suggestions for all groups.
    pub fn auto_suggest_all(&mut self) {
        self.actions = self.groups.iter().map(|g| g.suggested_actions()).collect();
    }
}

// ---------------------------------------------------------------------------
// Search state
// ---------------------------------------------------------------------------

/// A single match returned by the library search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Index into `Library::all_tracks` so we can jump to it.
    pub track_index: usize,
    /// Clone of the matched track (avoids borrow against the library).
    pub track: crate::library::Track,
    /// Human-readable labels for every field that matched the query,
    /// e.g. `["Artist", "Tag"]`.
    pub matched_fields: Vec<&'static str>,
}

/// Live-search overlay state.
#[derive(Debug)]
pub struct SearchState {
    /// Current query string (updated on every keystroke).
    pub query: String,
    /// Results for the current query, ordered by relevance then track order.
    pub results: Vec<SearchResult>,
    /// Selected row index within `results`.
    pub selected: usize,
}

impl SearchState {
    pub fn new() -> Self {
        Self { query: String::new(), results: Vec::new(), selected: 0 }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 { self.selected -= 1; }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.results.len() { self.selected += 1; }
    }

    pub fn page_up(&mut self, n: usize) {
        self.selected = self.selected.saturating_sub(n);
    }

    pub fn page_down(&mut self, n: usize) {
        let max = self.results.len().saturating_sub(1);
        self.selected = (self.selected + n).min(max);
    }
}

/// State for the tag-editing overlay.
#[derive(Debug)]
pub struct TagEditState {
    pub path: PathBuf,
    /// "Artist — Title" shown in the overlay header.
    pub display_name: String,
    /// Comma-separated tag string being edited.
    pub input: String,
}

// ---------------------------------------------------------------------------
// Amazon state
// ---------------------------------------------------------------------------

/// Which overlay (if any) is showing inside the Amazon screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmazonOverlay {
    /// Prompting the user to paste their amazon.com cookie string.
    CookieInput,
    /// Prompting the user to enter a download directory path.
    DirInput,
}

/// Which pane has focus in the Amazon side-by-side view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmazonFocus {
    /// Amazon catalog pane (left).
    Catalog,
    /// Local library pane (right).
    Local,
}

/// All mutable state for the Amazon easter egg screen.
pub struct AmazonState {
    /// Tracks fetched from the Amazon catalog.
    pub tracks: Vec<AmazonTrack>,
    /// Selected row in the Amazon catalog pane.
    pub catalog_index: usize,
    /// Selected row in the local library pane.
    pub local_index: usize,
    /// Which pane currently has keyboard focus.
    pub focus: AmazonFocus,
    /// ASINs currently being downloaded.
    pub downloading: std::collections::HashSet<String>,
    /// Download progress: ASIN → (bytes received, total bytes).
    pub progress: std::collections::HashMap<String, (u64, Option<u64>)>,
    /// ASINs already confirmed present on disk.
    pub completed: std::collections::HashSet<String>,
    /// Status / error message shown at the bottom of the pane.
    pub status: String,
    /// True while the initial catalog fetch is running.
    pub loading: bool,
    /// Set to true to signal the event loop to spawn a catalog fetch task.
    pub needs_fetch: bool,
    /// Active text-input overlay, if any.
    pub overlay: Option<AmazonOverlay>,
}

impl AmazonState {
    pub fn new_loading() -> Self {
        Self {
            tracks: Vec::new(),
            catalog_index: 0,
            local_index: 0,
            focus: AmazonFocus::Catalog,
            downloading: Default::default(),
            progress: Default::default(),
            completed: Default::default(),
            status: "Fetching catalog…".into(),
            loading: true,
            needs_fetch: true,
            overlay: None,
        }
    }

    pub fn new_with_overlay(overlay: AmazonOverlay) -> Self {
        Self {
            tracks: Vec::new(),
            catalog_index: 0,
            local_index: 0,
            focus: AmazonFocus::Catalog,
            downloading: Default::default(),
            progress: Default::default(),
            completed: Default::default(),
            status: String::new(),
            loading: false,
            needs_fetch: false,
            overlay: Some(overlay),
        }
    }

    pub fn move_catalog_up(&mut self) {
        if self.catalog_index > 0 {
            self.catalog_index -= 1;
        }
    }

    pub fn move_catalog_down(&mut self) {
        if !self.tracks.is_empty() && self.catalog_index + 1 < self.tracks.len() {
            self.catalog_index += 1;
        }
    }

    pub fn move_local_up(&mut self, count: usize) {
        if self.local_index > 0 {
            self.local_index = self.local_index.saturating_sub(count.max(1));
        }
    }

    pub fn move_local_down(&mut self, count: usize) {
        // upper bound set by caller using actual track count
        self.local_index = self.local_index.saturating_add(count.max(1));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Focus {
    Library,
    Devices,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub running: bool,
    pub screen: Screen,
    pub focus: Focus,

    pub library: Library,
    pub source_dirs: Vec<PathBuf>,
    pub sources_selected: usize,

    pub devices: Vec<Arc<dyn MusicDevice>>,
    pub selected_device: usize,

    /// Indices into `library.tracks` marked for transfer.
    pub selected_tracks: Vec<usize>,

    pub player: Player,

    pub transfer: TransferEngine,
    pub transfer_log: Vec<String>,

    // --- playlist state ---
    /// All known playlist names (refreshed when entering Playlists screen).
    pub playlist_names: Vec<String>,
    pub playlists_selected: usize,
    /// Context for a pending save-name collision.
    pub conflict_ctx: Option<ConflictCtx>,

    // --- shared text input ---
    pub input_buffer: String,

    pub status_message: Option<String>,

    // --- iPod repair ---
    pub repair_results: Option<DeviceScanResult>,
    pub repair_selected: usize,

    // --- device track browser ---
    pub device_tracks: Vec<DeviceTrackEntry>,
    pub device_tracks_selected: usize,

    // --- tag editor ---
    pub edit_state: Option<EditState>,

    // --- deduplication ---
    pub dedup_state: Option<DedupState>,

    // --- waveform visualizer ---
    /// When `true`, the library pane is replaced by the oscilloscope.
    pub waveform_active: bool,

    // --- tags ---
    pub tag_store: TagStore,
    /// Reverse index: track path → playlists that contain it.
    pub playlist_membership: HashMap<PathBuf, Vec<String>>,
    /// Active tag-editing overlay, if any.
    pub tag_edit_state: Option<TagEditState>,

    // --- search ---
    /// Live search overlay; `Some` while the search UI is open.
    pub search_state: Option<SearchState>,

    // --- marquee scrolling ---
    /// Monotonically incrementing tick counter, reset whenever the focused
    /// library track changes.  The UI computes the scroll offset from this.
    pub marquee_tick: u32,

    // --- Amazon easter egg ---
    /// Key sequence buffer for the A→C→E activation chord.
    pub amazon_key_seq: Vec<char>,
    /// Timestamp of the first key in the current sequence.
    pub amazon_key_seq_time: Option<std::time::Instant>,
    /// Active Amazon screen state (Some while Screen::Amazon is active).
    pub amazon_state: Option<AmazonState>,
    /// Shared inbox: async download tasks push messages here; the event loop
    /// drains it each tick and applies updates.
    pub amazon_inbox: std::sync::Arc<std::sync::Mutex<Vec<AmazonMsg>>>,
    /// Cookie string persisted from Config.
    pub amazon_cookie: Option<String>,
    /// Directory where downloaded MP3s are saved, persisted from Config.
    pub amazon_download_dir: Option<PathBuf>,
}

impl App {
    pub fn new(
        source_dirs: Vec<PathBuf>,
        audio_handle: Option<OutputStreamHandle>,
        amazon_cookie: Option<String>,
        amazon_download_dir: Option<PathBuf>,
    ) -> Self {
        let mut app = Self {
            running: true,
            screen: Screen::Library,
            focus: Focus::Library,
            library: Library::default(),
            source_dirs,
            sources_selected: 0,
            devices: Vec::new(),
            selected_device: 0,
            selected_tracks: Vec::new(),
            player: Player::new(audio_handle),
            transfer: TransferEngine::new(),
            transfer_log: Vec::new(),
            playlist_names: Vec::new(),
            playlists_selected: 0,
            conflict_ctx: None,
            input_buffer: String::new(),
            status_message: None,
            repair_results: None,
            repair_selected: 0,
            device_tracks: Vec::new(),
            device_tracks_selected: 0,
            edit_state: None,
            dedup_state: None,
            waveform_active: false,
            marquee_tick: 0,
            tag_store: TagStore::load(),
            playlist_membership: HashMap::new(),
            tag_edit_state: None,
            search_state: None,
            amazon_key_seq: Vec::new(),
            amazon_key_seq_time: None,
            amazon_state: None,
            amazon_inbox: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            amazon_cookie,
            amazon_download_dir,
        };
        app.rescan();
        app.rebuild_playlist_membership();
        app.sync_tag_sort_keys();
        app
    }

    // --- library ---

    pub fn rescan(&mut self) {
        let active_playlist = self.library.active_playlist.clone();
        match scanner::scan_directories(&self.source_dirs) {
            Ok(tracks) => {
                let n = tracks.len();
                self.library = Library::new(tracks);
                // Re-apply playlist filter if one was active
                if let Some(name) = active_playlist {
                    if let Ok(pl) = Playlist::load(&name) {
                        self.library.load_playlist(&name, &pl.tracks);
                    }
                }
                self.status_message = Some(format!(
                    "Scanned {} source(s) — {n} tracks.",
                    self.source_dirs.len()
                ));
                self.rebuild_playlist_membership();
                self.sync_tag_sort_keys();
            }
            Err(e) => self.status_message = Some(format!("Scan error: {e}")),
        }
    }

    // --- source dirs ---

    pub fn add_source(&mut self, path: PathBuf) {
        if !path.is_dir() {
            #[cfg(target_os = "android")]
            let hint = if path.starts_with("/storage") || path.starts_with("/sdcard") {
                "  Tip: run 'termux-setup-storage' in Termux first, then retry."
            } else {
                ""
            };
            #[cfg(not(target_os = "android"))]
            let hint = "";
            self.status_message = Some(format!("Not a directory: {}{hint}", path.display()));
            return;
        }
        if self.source_dirs.contains(&path) {
            self.status_message = Some(format!("Already a source: {}", path.display()));
            return;
        }
        self.source_dirs.push(path);
        self.rescan();
    }

    pub fn remove_selected_source(&mut self) {
        if self.source_dirs.is_empty() {
            return;
        }
        let removed = self.source_dirs.remove(self.sources_selected);
        if self.sources_selected > 0 && self.sources_selected >= self.source_dirs.len() {
            self.sources_selected -= 1;
        }
        self.status_message = Some(format!("Removed: {}", removed.display()));
        self.rescan();
    }

    pub fn sources_move_up(&mut self) {
        if self.sources_selected > 0 {
            self.sources_selected -= 1;
        }
    }

    pub fn sources_move_down(&mut self) {
        if self.sources_selected + 1 < self.source_dirs.len() {
            self.sources_selected += 1;
        }
    }

    pub fn sources_page_up(&mut self, page: usize) {
        self.sources_selected = self.sources_selected.saturating_sub(page);
    }

    pub fn sources_page_down(&mut self, page: usize) {
        let max = self.source_dirs.len().saturating_sub(1);
        self.sources_selected = (self.sources_selected + page).min(max);
    }

    // --- playlists ---

    pub fn refresh_playlist_names(&mut self) {
        self.playlist_names = Playlist::list_all();
        self.playlists_selected = self
            .playlists_selected
            .min(self.playlist_names.len().saturating_sub(1));
    }

    pub fn playlists_move_up(&mut self) {
        if self.playlists_selected > 0 {
            self.playlists_selected -= 1;
        }
    }

    pub fn playlists_move_down(&mut self) {
        if self.playlists_selected + 1 < self.playlist_names.len() {
            self.playlists_selected += 1;
        }
    }

    pub fn playlists_page_up(&mut self, page: usize) {
        self.playlists_selected = self.playlists_selected.saturating_sub(page);
    }

    pub fn playlists_page_down(&mut self, page: usize) {
        let max = self.playlist_names.len().saturating_sub(1);
        self.playlists_selected = (self.playlists_selected + page).min(max);
    }

    /// Load the highlighted playlist into the library view.
    pub fn load_selected_playlist(&mut self) {
        let Some(name) = self.playlist_names.get(self.playlists_selected).cloned() else {
            return;
        };
        match Playlist::load(&name) {
            Ok(pl) => {
                self.library.load_playlist(&name, &pl.tracks);
                self.screen = Screen::Library;
                self.status_message = Some(format!(
                    "Playlist '{}' loaded ({} tracks).",
                    pl.name,
                    pl.tracks.len()
                ));
            }
            Err(e) => self.status_message = Some(format!("Could not load playlist: {e}")),
        }
    }

    /// Delete the highlighted playlist.
    pub fn delete_selected_playlist(&mut self) {
        let Some(name) = self.playlist_names.get(self.playlists_selected).cloned() else {
            return;
        };
        match Playlist::delete(&name) {
            Ok(_) => {
                self.status_message = Some(format!("Deleted playlist '{name}'."));
                self.refresh_playlist_names();
                self.rebuild_playlist_membership();
            }
            Err(e) => self.status_message = Some(format!("Delete failed: {e}")),
        }
    }

    /// Begin the save-playlist flow for the currently selected tracks.
    pub fn begin_save_playlist(&mut self) {
        if self.selected_tracks.is_empty() {
            self.status_message = Some("Select tracks first (Space), then press W.".into());
            return;
        }
        self.input_buffer.clear();
        self.screen = Screen::SavePlaylist;
    }

    /// Called when the user presses Enter in SavePlaylist.
    pub fn confirm_save_playlist(&mut self) {
        let name = self.input_buffer.trim().to_string();
        self.input_buffer.clear();

        if name.is_empty() {
            self.status_message = Some("Playlist name cannot be empty.".into());
            self.screen = Screen::Library;
            return;
        }

        let new_tracks: Vec<PathBuf> = self
            .selected_tracks
            .iter()
            .filter_map(|&i| self.library.tracks.get(i).map(|t| t.path.clone()))
            .collect();

        if Playlist::exists(&name) {
            // Conflict — load the existing playlist and prompt
            let existing_tracks = Playlist::load(&name)
                .map(|p| p.tracks)
                .unwrap_or_default();
            self.conflict_ctx = Some(ConflictCtx { name, new_tracks, existing_tracks });
            self.screen = Screen::PlaylistConflict;
        } else {
            match Playlist::new(&name, new_tracks).save() {
                Ok(_) => {
                    self.clear_selection();
                    self.status_message = Some(format!("Playlist '{name}' saved."));
                    self.screen = Screen::Library;
                    self.rebuild_playlist_membership();
                }
                Err(e) => {
                    self.status_message = Some(format!("Save failed: {e}"));
                    self.screen = Screen::Library;
                }
            }
        }
    }

    /// Resolve conflict by overwriting the existing playlist.
    pub fn conflict_overwrite(&mut self) {
        let Some(ctx) = self.conflict_ctx.take() else { return };
        match ctx.resolve_overwrite() {
            Ok(_) => {
                self.clear_selection();
                self.status_message = Some(format!("Playlist '{}' overwritten.", ctx.name));
            }
            Err(e) => self.status_message = Some(format!("Save failed: {e}")),
        }
        self.screen = Screen::Library;
    }

    /// Resolve conflict by creating a new date-tagged merged playlist.
    pub fn conflict_new_dated(&mut self) {
        let Some(ctx) = self.conflict_ctx.take() else { return };
        match ctx.resolve_new_dated() {
            Ok(new_name) => {
                self.clear_selection();
                self.status_message = Some(format!("Saved as '{new_name}'."));
            }
            Err(e) => self.status_message = Some(format!("Save failed: {e}")),
        }
        self.screen = Screen::Library;
    }

    // --- device helpers ---

    pub fn selected_device_ref(&self) -> Option<Arc<dyn MusicDevice>> {
        self.devices.get(self.selected_device).cloned()
    }

    pub fn move_device_up(&mut self) {
        if self.selected_device > 0 {
            self.selected_device -= 1;
        }
    }

    pub fn move_device_down(&mut self) {
        if self.selected_device + 1 < self.devices.len() {
            self.selected_device += 1;
        }
    }

    // --- track selection ---

    pub fn toggle_selected_track(&mut self) {
        let idx = self.library.selected_index;
        if let Some(pos) = self.selected_tracks.iter().position(|&i| i == idx) {
            self.selected_tracks.remove(pos);
        } else {
            self.selected_tracks.push(idx);
        }
    }

    pub fn is_track_selected(&self, idx: usize) -> bool {
        self.selected_tracks.contains(&idx)
    }

    pub fn clear_selection(&mut self) {
        self.selected_tracks.clear();
    }

    // --- playback ---

    pub fn play_focused(&mut self) {
        if let Some(track) = self.library.tracks.get(self.library.selected_index).cloned() {
            if let Err(e) = self.player.play(&track) {
                self.status_message = Some(format!("Playback error: {e}"));
            }
        }
    }

    // --- transfer ---

    pub fn start_transfer(&mut self) {
        // Always open the Transfer screen so the log is visible.
        self.screen = Screen::Transfer;

        let Some(device) = self.selected_device_ref() else {
            self.status_message = Some("No device selected — showing log.".into());
            return;
        };
        if self.selected_tracks.is_empty() {
            self.status_message = Some("No tracks selected (Space to select) — showing log.".into());
            return;
        }
        let tracks: Vec<_> = self
            .selected_tracks
            .iter()
            .filter_map(|&i| self.library.tracks.get(i))
            .cloned()
            .collect();
        self.transfer.start_batch(device, tracks);
        self.status_message = Some(format!(
            "Transfer started ({} tracks).",
            self.selected_tracks.len()
        ));
        self.clear_selection();
    }

    // --- iPod repair ---

    /// Clone the Arc for the selected device if it is an `IpodUmsDevice`.
    fn selected_ipod_ums_arc(&self) -> Option<Arc<IpodUmsDevice>> {
        let arc = self.devices.get(self.selected_device)?.clone();
        // Arc<dyn MusicDevice> → try downcast to Arc<IpodUmsDevice>
        // We first get a raw ptr via as_any, confirm the type, then construct the Arc.
        if arc.as_any().is::<IpodUmsDevice>() {
            // Safety: we just confirmed the concrete type.
            let raw = Arc::into_raw(arc) as *const IpodUmsDevice;
            Some(unsafe { Arc::from_raw(raw) })
        } else {
            None
        }
    }

    /// Dump the contents of the iTunesDB on the selected iPod to the transfer log.
    pub fn dump_device_db(&mut self) {
        let Some(dev) = self.selected_ipod_ums_arc() else {
            self.status_message = Some("No iPod selected.".into());
            return;
        };
        self.screen = Screen::Transfer;
        self.transfer_log.push("── iTunesDB dump ──────────────────────────────────────".into());

        let tracks = dev.list_tracks();
        if tracks.is_empty() {
            self.transfer_log.push("  (no tracks found in database)".into());
        } else {
            self.transfer_log.push(format!("  {} track(s) in database:", tracks.len()));
            for (i, t) in tracks.iter().enumerate() {
                let src = if t.from_db { "DB" } else { "FS" };
                self.transfer_log.push(format!(
                    "  [{src}] #{i}  title={:?}  artist={:?}",
                    t.title, t.artist
                ));
                self.transfer_log.push(format!("         path={}", t.ipod_rel_path));
            }
        }
        self.transfer_log.push("──────────────────────────────────────────────────────".into());
    }

    /// Create a fresh iTunesDB on the selected iPod if none exists.
    pub fn init_device_database(&mut self) {
        let Some(dev) = self.selected_ipod_ums_arc() else {
            self.status_message = Some("No iPod selected.".into());
            return;
        };
        match dev.init_database() {
            Ok(()) => {
                self.status_message = Some(
                    "iTunesDB initialised — device is ready for transfers.".into(),
                );
                self.transfer_log.push(
                    "✓ Fresh iTunesDB created. You can now transfer tracks without iTunes.".into(),
                );
            }
            Err(e) => {
                self.status_message = Some(format!("Init failed: {e}"));
            }
        }
    }

    /// Scan the selected iPod for database issues and switch to the repair screen.
    pub fn scan_device_health(&mut self) {
        let Some(dev) = self.selected_ipod_ums_arc() else {
            self.status_message = Some("No iPod selected or device does not support repair.".into());
            return;
        };
        match dev.scan_health() {
            Ok(results) => {
                let issues = results.issue_count();
                self.status_message = Some(if issues == 0 {
                    "Device is healthy — no issues found.".into()
                } else {
                    format!("{issues} issue(s) found. Press F to fix all, Esc to cancel.")
                });
                self.repair_results = Some(results);
                self.repair_selected = 0;
                self.screen = Screen::RepairIpod;
            }
            Err(e) => self.status_message = Some(format!("Scan failed: {e}")),
        }
    }

    /// Repair all issues found in the last scan.
    pub fn repair_all_issues(&mut self) {
        let Some(dev) = self.selected_ipod_ums_arc() else { return };
        let Some(results) = self.repair_results.take() else { return };

        let mut fixed = 0usize;
        let mut errors: Vec<String> = Vec::new();

        for entry in &results.incomplete_entries {
            match dev.repair_incomplete(entry) {
                Ok(_) => fixed += 1,
                Err(e) => errors.push(format!("Repair failed (id={}): {e}", entry.track_id)),
            }
        }
        for orphan in &results.orphaned_files {
            match dev.repair_orphan(orphan) {
                Ok(_) => fixed += 1,
                Err(e) => errors.push(format!("Orphan repair failed: {e}")),
            }
        }

        let failed = errors.len();
        self.transfer_log.extend(errors);
        self.screen = Screen::Library;
        self.status_message = Some(format!(
            "Repair complete: {fixed} fixed, {failed} failed. Safely eject to apply."
        ));
    }

    // --- device track browser ---

    /// Load the track list from the selected iPod and switch to the browser screen.
    pub fn load_device_tracks(&mut self) {
        let Some(dev) = self.selected_ipod_ums_arc() else {
            self.status_message = Some("No iPod selected.".into());
            return;
        };
        let tracks = dev.list_tracks();
        let n = tracks.len();
        let from_db = tracks.first().map(|t| t.from_db).unwrap_or(false);

        if !from_db {
            // DB not found — run the diagnostic and push results to the transfer log
            // so the user can see exactly what was searched by pressing [T].
            let diag = dev.diagnose_db_location();
            self.transfer_log.push("── DB location diagnostic ──────────────────────".into());
            self.transfer_log.extend(diag);
            self.transfer_log.push("────────────────────────────────────────────────".into());
        }

        self.device_tracks = tracks;
        self.device_tracks_selected = 0;
        self.screen = Screen::DeviceTracks;
        self.status_message = Some(if from_db {
            format!("{n} tracks found on device (via iTunesDB).")
        } else {
            format!(
                "{n} tracks found (filesystem scan — iTunesDB not found). \
                 Press [Esc] then [T] for diagnostic."
            )
        });
    }

    pub fn device_tracks_move_up(&mut self) {
        if self.device_tracks_selected > 0 {
            self.device_tracks_selected -= 1;
        }
    }

    pub fn device_tracks_move_down(&mut self) {
        if self.device_tracks_selected + 1 < self.device_tracks.len() {
            self.device_tracks_selected += 1;
        }
    }

    pub fn device_tracks_page_up(&mut self, page: usize) {
        self.device_tracks_selected = self.device_tracks_selected.saturating_sub(page);
    }

    pub fn device_tracks_page_down(&mut self, page: usize) {
        let max = self.device_tracks.len().saturating_sub(1);
        self.device_tracks_selected = (self.device_tracks_selected + page).min(max);
    }

    pub fn repair_move_up(&mut self) {
        if self.repair_selected > 0 {
            self.repair_selected -= 1;
        }
    }

    pub fn repair_move_down(&mut self) {
        let max = self.repair_results.as_ref().map(|r| r.issue_count()).unwrap_or(0);
        if self.repair_selected + 1 < max {
            self.repair_selected += 1;
        }
    }

    pub fn repair_page_up(&mut self, page: usize) {
        self.repair_selected = self.repair_selected.saturating_sub(page);
    }

    pub fn repair_page_down(&mut self, page: usize) {
        let max = self.repair_results.as_ref().map(|r| r.issue_count()).unwrap_or(0).saturating_sub(1);
        self.repair_selected = (self.repair_selected + page).min(max);
    }

    // --- tag editor ---

    /// Open the tag editor for the currently focused library track.
    pub fn begin_edit(&mut self) {
        let Some(track) = self.library.tracks.get(self.library.selected_index) else {
            self.status_message = Some("No track focused.".into());
            return;
        };
        self.edit_state = Some(EditState {
            path: track.path.clone(),
            fields: [
                track.title.clone(),
                track.artist.clone(),
                track.album.clone(),
                track.year.map(|y| y.to_string()).unwrap_or_default(),
                String::new(), // genre — not stored in Track; leave blank
            ],
            focused_field: 0,
        });
        self.screen = Screen::EditTrack;
    }

    /// Commit the edits to file and update the in-memory library.
    pub fn confirm_edit(&mut self) {
        let Some(state) = self.edit_state.take() else { return };

        let edit = TrackEdit {
            title:        Some(state.fields[0].clone()),
            artist:       Some(state.fields[1].clone()),
            album:        Some(state.fields[2].clone()),
            year:         state.fields[3].trim().parse().ok(),
            genre:        {
                let g = state.fields[4].trim().to_string();
                if g.is_empty() { None } else { Some(g) }
            },
            track_number: None,
        };

        match self.library.apply_edit(&state.path, &edit) {
            Ok(()) => self.status_message = Some("Tags saved.".into()),
            Err(e) => self.status_message = Some(format!("Save failed: {e}")),
        }
        self.screen = Screen::Library;
    }

    /// Discard edits and return to the library.
    pub fn cancel_edit(&mut self) {
        self.edit_state = None;
        self.screen = Screen::Library;
    }

    pub fn edit_type_char(&mut self, c: char) {
        if let Some(state) = &mut self.edit_state {
            state.fields[state.focused_field].push(c);
        }
    }

    pub fn edit_backspace(&mut self) {
        if let Some(state) = &mut self.edit_state {
            state.fields[state.focused_field].pop();
        }
    }

    pub fn reset_marquee(&mut self) {
        self.marquee_tick = 0;
    }

    pub fn edit_next_field(&mut self) {
        if let Some(state) = &mut self.edit_state {
            state.focused_field = (state.focused_field + 1) % EDIT_FIELD_LABELS.len();
        }
    }

    pub fn edit_prev_field(&mut self) {
        if let Some(state) = &mut self.edit_state {
            let n = EDIT_FIELD_LABELS.len();
            state.focused_field = if state.focused_field == 0 { n - 1 } else { state.focused_field - 1 };
        }
    }

    // --- deduplication ---

    /// Scan the library for duplicates and open the dedup screen.
    pub fn begin_dedup(&mut self) {
        let groups = dedup::find_duplicates(&self.library.all_tracks);
        let n = groups.len();
        if groups.is_empty() {
            self.status_message = Some("No duplicates found in library.".into());
            return;
        }
        self.status_message = Some(format!(
            "Found {n} duplicate group(s). Review and mark tracks to delete."
        ));
        self.dedup_state = Some(DedupState::new(groups));
        self.screen = Screen::Dedup;
    }

    /// Delete all tracks marked Delete, remove them from the library, and
    /// return to the library screen.
    pub fn apply_dedup(&mut self) {
        let Some(state) = self.dedup_state.take() else { return };

        let mut deleted = 0usize;
        let mut errors: Vec<String> = Vec::new();
        let mut deleted_paths = std::collections::HashSet::new();

        for (group, actions) in state.groups.iter().zip(state.actions.iter()) {
            for (candidate, &action) in group.candidates.iter().zip(actions.iter()) {
                if action == DedupAction::Delete {
                    match std::fs::remove_file(&candidate.track.path) {
                        Ok(()) => {
                            deleted += 1;
                            deleted_paths.insert(candidate.track.path.clone());
                        }
                        Err(e) => errors.push(format!(
                            "Could not delete {}: {e}",
                            candidate.track.path.display()
                        )),
                    }
                }
            }
        }

        // Remove deleted tracks from in-memory library.
        self.library.all_tracks.retain(|t| !deleted_paths.contains(&t.path));
        self.library.tracks.retain(|t| !deleted_paths.contains(&t.path));
        self.library.selected_index =
            self.library.selected_index.min(self.library.tracks.len().saturating_sub(1));

        self.screen = Screen::Library;
        let failed = errors.len();
        for e in errors {
            self.transfer_log.push(e);
        }
        self.status_message = Some(format!(
            "Dedup: {deleted} file(s) deleted{}.",
            if failed > 0 { format!(", {failed} failed — see transfer log") } else { String::new() }
        ));
    }

    /// Discard dedup state and return to library without deleting anything.
    pub fn cancel_dedup(&mut self) {
        self.dedup_state = None;
        self.screen = Screen::Library;
        self.status_message = Some("Deduplication cancelled — no files deleted.".into());
    }

    // --- tags ---

    /// Rebuild the path → [playlist name, …] reverse index from all saved playlists.
    pub fn rebuild_playlist_membership(&mut self) {
        let names = crate::playlist::Playlist::list_all();
        let mut membership: HashMap<PathBuf, Vec<String>> = HashMap::new();
        for name in &names {
            if let Ok(pl) = crate::playlist::Playlist::load(name) {
                for path in pl.tracks {
                    membership.entry(path).or_default().push(name.clone());
                }
            }
        }
        self.playlist_membership = membership;
    }

    /// Sync first-tag-per-track keys into the library (for GroupByTag sort/sections).
    pub fn sync_tag_sort_keys(&mut self) {
        let keys: HashMap<PathBuf, String> = self.library.all_tracks.iter()
            .filter_map(|t| {
                self.tag_store.tags_for(&t.path).into_iter().next()
                    .map(|first| (t.path.clone(), first))
            })
            .collect();
        self.library.set_tag_sort_keys(keys);
    }

    /// Open the tag-editing overlay for the currently focused track.
    pub fn begin_tag_edit(&mut self) {
        if let Some(track) = self.library.selected() {
            let current = self.tag_store.tags_for(&track.path).join(", ");
            let display_name = format!("{} — {}", track.display_artist(), track.display_title());
            self.tag_edit_state = Some(TagEditState {
                path: track.path.clone(),
                display_name,
                input: current,
            });
        }
    }

    /// Confirm and save the tag edit.
    pub fn confirm_tag_edit(&mut self) {
        if let Some(state) = self.tag_edit_state.take() {
            let tags: Vec<String> = state.input
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            self.tag_store.set_tags(&state.path, tags);
            self.tag_store.save();
            self.sync_tag_sort_keys();
            if self.library.sort_order == crate::library::SortOrder::GroupByTag {
                self.library.apply_sort();
            }
            self.status_message = Some("Tags saved.".into());
        }
    }

    /// Cancel the tag edit without saving.
    pub fn cancel_tag_edit(&mut self) {
        self.tag_edit_state = None;
    }

    // --- search ---

    /// Open the search overlay with an empty query.
    pub fn begin_search(&mut self) {
        self.search_state = Some(SearchState::new());
    }

    /// Append a character to the query and re-run the search.
    pub fn search_push(&mut self, c: char) {
        if let Some(state) = &mut self.search_state {
            state.query.push(c);
            state.selected = 0;
        }
        self.run_search();
    }

    /// Remove the last character from the query and re-run the search.
    pub fn search_pop(&mut self) {
        if let Some(state) = &mut self.search_state {
            state.query.pop();
            state.selected = 0;
        }
        self.run_search();
    }

    /// Navigate to the selected result in the library and close the overlay.
    pub fn confirm_search(&mut self) {
        let Some(state) = &self.search_state else { return };
        let Some(result) = state.results.get(state.selected) else {
            self.search_state = None;
            return;
        };
        let target_path = result.track.path.clone();
        if let Some(pos) = self.library.tracks.iter().position(|t| t.path == target_path) {
            self.library.selected_index = pos;
            self.reset_marquee();
        } else {
            // Track filtered out by playlist — clear filter first.
            self.library.clear_playlist();
            if let Some(pos) = self.library.tracks.iter().position(|t| t.path == target_path) {
                self.library.selected_index = pos;
                self.reset_marquee();
            }
        }
        self.search_state = None;
    }

    /// Close the search overlay without navigating.
    pub fn cancel_search(&mut self) {
        self.search_state = None;
    }

    /// Run the current query against all tracks and update results.
    fn run_search(&mut self) {
        let Some(state) = &mut self.search_state else { return };
        let query = state.query.trim().to_lowercase();

        if query.is_empty() {
            state.results.clear();
            return;
        }

        let mut results: Vec<SearchResult> = self
            .library
            .all_tracks
            .iter()
            .enumerate()
            .filter_map(|(idx, track)| {
                let mut fields: Vec<&'static str> = Vec::new();

                if track.title.to_lowercase().contains(&query) { fields.push("Title"); }
                if track.artist.to_lowercase().contains(&query) { fields.push("Artist"); }
                if track.album.to_lowercase().contains(&query) { fields.push("Album"); }
                if track.year.map(|y| y.to_string()).as_deref().unwrap_or("").contains(&query) {
                    fields.push("Year");
                }
                // File stem — only shown if title didn't already match
                if !fields.contains(&"Title") {
                    if track.path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_lowercase().contains(&query))
                        .unwrap_or(false)
                    {
                        fields.push("File");
                    }
                }
                // User tags
                if self.tag_store.tags_for(&track.path)
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query))
                {
                    fields.push("Tag");
                }
                // Playlist membership
                if self.playlist_membership
                    .get(&track.path)
                    .map(|plists| plists.iter().any(|p| p.to_lowercase().contains(&query)))
                    .unwrap_or(false)
                {
                    fields.push("Playlist");
                }

                if fields.is_empty() {
                    None
                } else {
                    Some(SearchResult {
                        track_index: idx,
                        track: track.clone(),
                        matched_fields: fields,
                    })
                }
            })
            .collect();

        // Exact title match → top; exact artist → next; rest in scan order.
        results.sort_by_key(|r| {
            let exact_title  = r.track.title.to_lowercase()  == query;
            let exact_artist = r.track.artist.to_lowercase() == query;
            (!exact_title, !exact_artist, r.track_index)
        });

        state.results = results;
    }

    // --- Amazon easter egg ---

    /// Called when the A→C→E sequence completes.
    /// Shows the cookie input overlay if no cookie is set, the dir prompt if no
    /// download dir is set, otherwise opens the catalog view and starts fetching.
    pub fn activate_amazon(&mut self) {
        if self.amazon_cookie.is_none() {
            self.input_buffer.clear();
            self.amazon_state = Some(AmazonState::new_with_overlay(AmazonOverlay::CookieInput));
        } else if self.amazon_download_dir.is_none() {
            self.input_buffer.clear();
            self.amazon_state = Some(AmazonState::new_with_overlay(AmazonOverlay::DirInput));
        } else {
            self.amazon_state = Some(AmazonState::new_loading());
        }
        self.screen = Screen::Amazon;
    }

    /// Confirm the cookie string entered in the overlay.
    pub fn confirm_amazon_cookie(&mut self, cfg: &mut crate::config::Config) {
        let cookie = self.input_buffer.trim().to_string();
        self.input_buffer.clear();
        if cookie.is_empty() {
            return;
        }
        self.amazon_cookie = Some(cookie.clone());
        cfg.amazon_cookie = Some(cookie);
        cfg.save();

        if let Some(state) = &mut self.amazon_state {
            if self.amazon_download_dir.is_none() {
                state.overlay = Some(AmazonOverlay::DirInput);
            } else {
                state.overlay = None;
                state.loading = true;
                state.needs_fetch = true;
                state.status = "Fetching catalog…".into();
            }
        }
    }

    /// Confirm the download directory entered in the overlay.
    pub fn confirm_amazon_dir(&mut self, cfg: &mut crate::config::Config) {
        let dir = crate::util::expand_tilde(self.input_buffer.trim());
        self.input_buffer.clear();
        if dir.as_os_str().is_empty() {
            return;
        }
        self.amazon_download_dir = Some(dir.clone());
        cfg.amazon_download_dir = Some(dir);
        cfg.save();

        if let Some(state) = &mut self.amazon_state {
            state.overlay = None;
            state.loading = true;
            state.needs_fetch = true;
            state.status = "Fetching catalog…".into();
        }
    }

    /// Drain the amazon_inbox and apply any pending messages to amazon_state.
    pub fn drain_amazon_inbox(&mut self) {
        let msgs: Vec<AmazonMsg> = {
            let mut q = match self.amazon_inbox.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            q.drain(..).collect()
        };

        let Some(state) = &mut self.amazon_state else { return };

        for msg in msgs {
            match msg {
                AmazonMsg::Tracks(tracks) => {
                    state.loading = false;
                    let n = tracks.len();
                    state.tracks = tracks;
                    state.status = format!("{n} tracks in your Amazon library.");
                }
                AmazonMsg::Progress { asin, bytes, total } => {
                    state.progress.insert(asin, (bytes, total));
                }
                AmazonMsg::Downloaded { asin, path } => {
                    state.downloading.remove(&asin);
                    state.progress.remove(&asin);
                    state.completed.insert(asin);
                    state.status = format!("Downloaded: {}", path.file_name().unwrap_or_default().to_string_lossy());
                }
                AmazonMsg::Error(e) => {
                    state.loading = false;
                    state.status = format!("Error: {e}");
                }
            }
        }
    }

    // --- tick ---

    pub fn tick(&mut self) {
        self.player.tick();
        self.tick_transfer();
        self.drain_amazon_inbox();
        self.marquee_tick = self.marquee_tick.wrapping_add(1);
    }

    pub fn tick_transfer(&mut self) {
        while let Some(ev) = self.transfer.poll_event() {
            let line = match &ev {
                TransferEvent::Started { track_title, index, total } => {
                    format!("[{}/{}] Transferring: {}", index + 1, total, track_title)
                }
                TransferEvent::Progress { bytes_done, bytes_total } => {
                    format!("  ... {} / {} bytes", bytes_done, bytes_total)
                }
                TransferEvent::TrackDone { track_title, destination } => {
                    format!("  ✓ {} -> {}", track_title, destination)
                }
                TransferEvent::TrackFailed { track_title, reason } => {
                    if track_title.is_empty() {
                        format!("    {reason}")   // diagnostic / search log line
                    } else {
                        format!("  ✗ {} — {}", track_title, reason)
                    }
                }
                TransferEvent::BatchComplete { succeeded, failed } => {
                    format!("Done. {} succeeded, {} failed.", succeeded, failed)
                }
            };
            self.transfer_log.push(line);
        }
    }
}
