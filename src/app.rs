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
use crate::tags::TagStore;
use crate::transfer::{TransferEngine, TransferEvent};
use crate::organize::{OrganizerEngine, OrganizerEvent};
use crate::config::Config;
use crate::p2p::{P2pBufferState, P2pHandle, Toast, ToastLevel};
use crate::p2p::identity::PgpIdentity;
use crate::p2p::node::MusicNode;
use crate::p2p::party::PartyLineState;
use crate::p2p::trust::NodeInfo;
use crate::p2p::wire::RemoteTrack;

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
    /// File organizer — copy/verify/delete tracks into new folders.
    Organize,
    /// P2P peer management — approve/reject/view connected peers.  (Beta)
    P2pPeers,
    /// First-run identity entry — set an alphanumeric display name ≤12 chars.
    P2pIdentity,
    /// Text-input screen to connect to a peer by multiaddr.  (Beta)
    P2pConnect,
    /// Browse remote music libraries from trusted peers.  (Beta)
    RemoteLibrary,
    /// Party Line — nominate and vote on tracks for group playback.  (Beta)
    PartyLine,
    /// Configuration editor — live-editable settings with hot-reload.
    Settings,
}

// ---------------------------------------------------------------------------
// Settings state
// ---------------------------------------------------------------------------

/// A single editable settings field shown on the Settings screen.
#[derive(Debug, Clone)]
pub struct SettingsField {
    /// Human-readable label shown in the left column.
    pub label: &'static str,
    /// Brief description shown below the value when this field is focused.
    pub description: &'static str,
    /// The current value as an editable string.
    pub value: String,
    /// The config key this field maps to.
    pub key: &'static str,
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
// Gematria shuffle state
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Organizer state
// ---------------------------------------------------------------------------

/// A single group of tracks that can be moved together.
#[derive(Debug, Clone)]
pub struct OrganizerGroup {
    /// Display label (e.g. "Rock", "2024", "MP3").
    pub label: String,
    pub tracks: Vec<crate::library::Track>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrganizerPhase {
    /// User is browsing groups / selection and choosing what to move.
    PickGroup,
    /// User is typing the destination directory path.
    DestInput,
    /// Copy/verify/delete in progress.
    Running,
    /// All done — showing final log.
    Done,
}

pub struct OrganizerState {
    pub phase: OrganizerPhase,
    pub groups: Vec<OrganizerGroup>,
    pub group_index: usize,
    /// Pre-filled / in-progress destination path string.
    pub dest_input: String,
    pub log: Vec<String>,
    pub log_scroll: usize,
    pub results: Option<(usize, usize)>, // (succeeded, failed)
}

impl OrganizerState {
    pub fn new(groups: Vec<OrganizerGroup>) -> Self {
        Self {
            phase: OrganizerPhase::PickGroup,
            groups,
            group_index: 0,
            dest_input: String::new(),
            log: Vec::new(),
            log_scroll: 0,
            results: None,
        }
    }

    pub fn selected_group(&self) -> Option<&OrganizerGroup> {
        self.groups.get(self.group_index)
    }

    pub fn move_group_up(&mut self) {
        self.group_index = self.group_index.saturating_sub(1);
    }

    pub fn move_group_down(&mut self) {
        if !self.groups.is_empty() {
            self.group_index = (self.group_index + 1).min(self.groups.len() - 1);
        }
    }

    pub fn scroll_log_up(&mut self) {
        self.log_scroll = self.log_scroll.saturating_sub(1);
    }

    pub fn scroll_log_down(&mut self) {
        if !self.log.is_empty() {
            self.log_scroll = (self.log_scroll + 1).min(self.log.len() - 1);
        }
    }
}

/// State for the gematria track-selection overlay.
pub struct GematriaState {
    /// The phrase the user has typed so far.
    pub phrase: String,
    /// Computed results (one per system), populated after the user presses Enter.
    pub results: Vec<crate::gematria::SystemResult>,
    /// Which system is currently highlighted (cycled with Tab).
    pub selected_system: usize,
    /// Index of the track that would be selected by the highlighted system.
    pub track_index: Option<usize>,
}

impl GematriaState {
    pub fn new() -> Self {
        Self {
            phrase: String::new(),
            results: Vec::new(),
            selected_system: 0,
            track_index: None,
        }
    }

    /// Recompute results for the current phrase against `track_count`.
    pub fn compute(&mut self, track_count: usize) {
        if self.phrase.trim().is_empty() {
            self.results.clear();
            self.track_index = None;
            return;
        }
        self.results = crate::gematria::compute(&self.phrase);
        self.selected_system = self.selected_system.min(self.results.len().saturating_sub(1));
        self.track_index = self.results.get(self.selected_system).map(|r| {
            crate::gematria::select_index(r.total, track_count)
        });
    }

    pub fn cycle_system(&mut self, track_count: usize) {
        if self.results.is_empty() { return; }
        self.selected_system = (self.selected_system + 1) % self.results.len();
        self.track_index = self.results.get(self.selected_system).map(|r| {
            crate::gematria::select_index(r.total, track_count)
        });
    }
}

// ---------------------------------------------------------------------------
// Amazon state
// ---------------------------------------------------------------------------

/// Which overlay (if any) is showing inside the Amazon screen.
/// (Currently unused — kept for future use.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmazonOverlay {}

/// All mutable state for the Amazon easter egg screen.
pub struct AmazonState {
    /// Status message shown at the bottom of the pane.
    pub status: String,
    /// Local Amazon Music installation info (detected once at screen open).
    pub local: Option<crate::platform::AmazonMusicLocal>,
    /// Lines emitted by the CDP automation engine.
    pub cdp_log: Vec<String>,
    /// True while the CDP task is running.
    pub cdp_running: bool,
    /// Scroll offset for the CDP log pane (lines from bottom).
    pub cdp_log_scroll: usize,
    /// Receive end of the CDP automation channel (Windows only).
    #[cfg(target_os = "windows")]
    pub cdp_rx: Option<tokio::sync::mpsc::Receiver<crate::amazon_cdp::CdpMsg>>,
}

impl AmazonState {
    pub fn new() -> Self {
        Self {
            status: String::new(),
            local: None,
            cdp_log: Vec::new(),
            cdp_running: false,
            cdp_log_scroll: 0,
            #[cfg(target_os = "windows")]
            cdp_rx: None,
        }
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

    // --- file organizer ---
    pub organize: OrganizerEngine,
    pub organizer_state: Option<OrganizerState>,

    // --- gematria shuffle ---
    /// Active gematria selection overlay; `Some` while the input box is open.
    pub gematria_state: Option<GematriaState>,
    /// The phrase from the last gematria session — pre-filled on next open.
    pub last_gematria_phrase: String,

    // --- decoder error ---
    /// Set when the audio decoder panics mid-stream. Shows an overlay offering
    /// to remove the offending track from the library.
    pub decoder_error_track: Option<crate::library::Track>,

    // --- shuffle RNG ---
    /// Internal xorshift64 state, seeded from wall-clock at startup.
    /// Used only when shuffle is enabled to pick the next track.
    shuffle_rng: u64,

    // --- P2P music sharing (beta) ---
    /// Active P2P handle; `None` when P2P is inactive.
    pub p2p_node: Option<P2pHandle>,
    /// Buffering/playing state for the player pane P2P display.
    pub p2p_buffer_state: P2pBufferState,
    /// Non-modal toast queue rendered in the bottom-right corner.
    pub p2p_toasts: std::collections::VecDeque<Toast>,
    /// Merged catalog of tracks from all trusted peers.
    pub remote_tracks: Vec<RemoteTrack>,
    /// Party Line state — `Some` when the P2P screen has been opened at least once.
    pub party_line: Option<PartyLineState>,
    /// Snapshot of all known peers for the P2pPeers screen.
    pub p2p_peer_list: Vec<NodeInfo>,
    /// Scroll offset for the RemoteLibrary screen.
    pub remote_library_selected: usize,
    /// Scroll offset for the P2pPeers screen.
    pub p2p_peers_selected: usize,
    /// Our own confirmed P2P listen addresses (full multiaddrs including PeerId).
    pub p2p_listen_addrs: Vec<String>,
    /// Key sequence buffer for the `p`→`2`→`p` activation chord.
    pub p2p_key_seq: Vec<char>,
    /// Timestamp of the first key in the current P2P chord sequence.
    pub p2p_key_seq_time: Option<std::time::Instant>,

    // --- settings (hot-reloadable) ---
    /// Seconds without a chunk before flagging stalled (from config).
    pub p2p_stall_secs: u64,
    /// Seconds after stall before abandoning transfer (from config).
    pub p2p_abandon_secs: u64,

    // --- settings screen state ---
    /// Settings fields being edited: Vec of (label, current_value_string).
    pub settings_fields: Vec<SettingsField>,
    /// Which settings field is currently focused.
    pub settings_selected: usize,
    /// True when the focused settings field is in inline-edit mode.
    pub settings_editing: bool,
}

impl App {
    pub fn new(
        source_dirs: Vec<PathBuf>,
        audio_handle: Option<OutputStreamHandle>,
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
            organize: OrganizerEngine::new(),
            organizer_state: None,
            gematria_state: None,
            last_gematria_phrase: String::new(),
            decoder_error_track: None,
            shuffle_rng: {
                // Non-zero seed required by xorshift64.
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| (d.as_secs() << 20) ^ d.subsec_nanos() as u64)
                    .unwrap_or(0x853c49e6748fea9b);
                if t == 0 { 0x853c49e6748fea9b } else { t }
            },
            p2p_node: None,
            p2p_buffer_state: P2pBufferState::Idle,
            p2p_toasts: std::collections::VecDeque::new(),
            remote_tracks: Vec::new(),
            party_line: None,
            p2p_peer_list: Vec::new(),
            remote_library_selected: 0,
            p2p_peers_selected: 0,
            p2p_listen_addrs: Vec::new(),
            p2p_key_seq: Vec::new(),
            p2p_key_seq_time: None,
            p2p_stall_secs: 5,
            p2p_abandon_secs: 30,
            settings_fields: Vec::new(),
            settings_selected: 0,
            settings_editing: false,
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
                // Re-broadcast catalog to P2P peers after rescan
                if let Some(node) = &self.p2p_node {
                    let catalog = crate::p2p::catalog::build_catalog(&self.library);
                    node.send(crate::p2p::P2pCommand::AnnounceLibrary(catalog));
                    let paths = crate::p2p::catalog::build_path_map(&self.library);
                    node.send(crate::p2p::P2pCommand::SetLocalPaths(paths));
                }
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
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.player.play(&track)
            }));
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.status_message = Some(format!("Playback error: {e}")),
                Err(_) => self.status_message = Some(format!(
                    "Playback error: decoder panic for '{}' — file may be corrupt or unsupported.",
                    track.display_title()
                )),
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

    // --- Gematria shuffle ---

    /// Open the gematria selection overlay, pre-filling the last phrase.
    pub fn begin_gematria(&mut self) {
        let mut state = GematriaState::new();
        state.phrase = self.last_gematria_phrase.clone();
        let count = self.library.tracks.len();
        state.compute(count);
        self.gematria_state = Some(state);
    }

    pub fn gematria_push(&mut self, c: char) {
        if let Some(s) = &mut self.gematria_state {
            s.phrase.push(c);
            let count = self.library.tracks.len();
            s.compute(count);
        }
    }

    pub fn gematria_pop(&mut self) {
        if let Some(s) = &mut self.gematria_state {
            s.phrase.pop();
            let count = self.library.tracks.len();
            s.compute(count);
        }
    }

    pub fn gematria_cycle_system(&mut self) {
        let count = self.library.tracks.len();
        if let Some(s) = &mut self.gematria_state {
            s.cycle_system(count);
        }
    }

    /// Accept the current selection: jump to the track and play it.
    pub fn confirm_gematria(&mut self) {
        let Some(s) = &self.gematria_state else { return };
        if let Some(idx) = s.track_index {
            self.library.selected_index = idx.min(self.library.tracks.len().saturating_sub(1));
            self.last_gematria_phrase = s.phrase.clone();
            self.reset_marquee();
            self.play_focused();
        }
        self.gematria_state = None;
    }

    pub fn cancel_gematria(&mut self) {
        if let Some(s) = &self.gematria_state {
            if !s.phrase.is_empty() {
                self.last_gematria_phrase = s.phrase.clone();
            }
        }
        self.gematria_state = None;
    }

    // --- decoder error recovery ---

    /// Remove the errored track from the library and delete the file from disk.
    pub fn remove_decoder_error_track(&mut self) {
        let Some(track) = self.decoder_error_track.take() else { return };
        let path = track.path.clone();
        let _ = std::fs::remove_file(&path);
        self.library.all_tracks.retain(|t| t.path != path);
        self.library.tracks.retain(|t| t.path != path);
        self.library.selected_index =
            self.library.selected_index.min(self.library.tracks.len().saturating_sub(1));
        self.status_message = Some(format!(
            "Removed '{}' from library and disk.",
            track.display_title()
        ));
    }

    /// Dismiss the decoder error overlay without removing the track.
    pub fn dismiss_decoder_error(&mut self) {
        self.decoder_error_track = None;
    }

    // --- Amazon easter egg ---

    /// Called when the A→C→E sequence completes.
    pub fn activate_amazon(&mut self) {
        let local = crate::platform::detect_amazon_music();
        let status = if local.download_dir_exists {
            format!(
                "Local downloads: {}  —  [S] add as source  [L] launch app",
                local.download_dir.display()
            )
        } else if local.is_installed() {
            "[L] Launch Amazon Music to download your purchases, then [S] to import".into()
        } else {
            "Amazon Music not found. Install from amazon.com/music/unlimited/download".into()
        };
        self.amazon_state = Some(AmazonState { status, local: Some(local), ..AmazonState::new() });
        self.screen = Screen::Amazon;
    }

    /// Add the local Amazon Music download directory as a library source and rescan.
    pub fn add_amazon_local_source(&mut self, cfg: &mut crate::config::Config) {
        let dir = match self.amazon_state.as_ref().and_then(|s| s.local.as_ref()) {
            Some(local) if local.download_dir_exists => local.download_dir.clone(),
            _ => {
                if let Some(state) = &mut self.amazon_state {
                    state.status = "No local Amazon Music download directory found.".into();
                }
                return;
            }
        };

        if !self.source_dirs.contains(&dir) {
            self.source_dirs.push(dir.clone());
            cfg.source_dirs = self.source_dirs.clone();
            cfg.save();
            self.rescan();
            if let Some(state) = &mut self.amazon_state {
                state.status = format!("Added '{}' as a source — rescanning.", dir.display());
            }
        } else if let Some(state) = &mut self.amazon_state {
            state.status = format!("'{}' is already a source.", dir.display());
        }
    }

    /// Launch the Amazon Music desktop application (Windows/Linux/macOS only).
    pub fn launch_amazon_app(&mut self) {
        let launched = self.amazon_state.as_ref()
            .and_then(|s| s.local.as_ref())
            .map(|local| crate::platform::launch_amazon_music(local))
            .unwrap_or(false);

        if let Some(state) = &mut self.amazon_state {
            state.status = if launched {
                "Amazon Music app launched.".into()
            } else {
                "Could not launch Amazon Music app — is it installed?".into()
            };
        }
    }

    /// Start the CDP download automation (Win32 installs only).
    /// Spawns a background task and stores the receive channel in `AmazonState`.
    #[cfg(target_os = "windows")]
    pub fn start_cdp_download(&mut self) {
        let state = match &mut self.amazon_state {
            Some(s) => s,
            None => return,
        };

        // UWP installs can't accept CLI args — debug port won't open.
        if let Some(local) = &state.local {
            if local.is_uwp {
                state.cdp_log.push(
                    "CDP automation requires the Win32 installer, not the Store/UWP version."
                        .into(),
                );
                state.cdp_log.push(
                    "Download the classic installer from amazon.com/music/unlimited/download"
                        .into(),
                );
                return;
            }
            if local.exe.is_none() {
                state.cdp_log.push("Amazon Music Win32 exe not found.".into());
                return;
            }
        }

        if state.cdp_running {
            state.cdp_log.push("Automation already running...".into());
            return;
        }

        let exe = state
            .local
            .as_ref()
            .and_then(|l| l.exe.clone())
            .expect("checked above");

        state.cdp_log.clear();
        state.cdp_log_scroll = 0;
        state.cdp_running = true;
        state.cdp_rx = Some(crate::amazon_cdp::spawn_download_automation(exe));
    }

    /// Drain any pending CDP messages into the log. Called every tick.
    pub fn tick_amazon_cdp(&mut self) {
        #[cfg(target_os = "windows")]
        {
            use crate::amazon_cdp::CdpMsg;
            let state = match &mut self.amazon_state {
                Some(s) => s,
                None => return,
            };
            let rx = match &mut state.cdp_rx {
                Some(r) => r,
                None => return,
            };
            loop {
                match rx.try_recv() {
                    Ok(CdpMsg::Log(line)) => {
                        state.cdp_log.push(line);
                        state.cdp_log_scroll = 0; // stay at bottom
                    }
                    Ok(CdpMsg::Done) => {
                        state.cdp_running = false;
                        state.cdp_rx = None;
                        break;
                    }
                    Ok(CdpMsg::Error(e)) => {
                        state.cdp_log.push(format!("✗ {e}"));
                        state.cdp_running = false;
                        state.cdp_rx = None;
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // --- auto-advance (shuffle / sequential) ---

    /// Advance to the next track after one ends naturally.
    /// Sequential mode: moves to the next index, wrapping at the end of the list.
    /// Shuffle mode: picks a random index (different from current when possible).
    fn advance_track(&mut self) {
        let n = self.library.tracks.len();
        if n == 0 {
            return;
        }
        let next = if self.player.shuffle == crate::player::ShuffleMode::On {
            let current = self.library.selected_index;
            let idx = self.next_shuffle_index(n);
            // If we happened to land on the same track and there's more than one,
            // nudge forward one position.
            if idx == current && n > 1 { (idx + 1) % n } else { idx }
        } else {
            // Sequential: wrap around to the start after the last track.
            (self.library.selected_index + 1) % n
        };
        self.library.selected_index = next;
        self.reset_marquee();
        self.play_focused();
    }

    /// xorshift64 — returns a value in `0..n`.
    fn next_shuffle_index(&mut self, n: usize) -> usize {
        let mut x = self.shuffle_rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.shuffle_rng = x;
        (x as usize) % n
    }

    // --- tick ---

    pub fn tick(&mut self) {
        self.player.tick();
        if let Some(track) = self.player.take_decoder_panic() {
            self.decoder_error_track = Some(track);
            // Suppress any pending advance so the error overlay can show cleanly.
            self.player.needs_next = false;
        } else if self.player.take_needs_next() {
            // Only auto-advance into the local library when no remote track is
            // active.  If a remote track just finished, clear the buffer state
            // and stop — the user must choose the next remote track explicitly.
            match &self.p2p_buffer_state {
                P2pBufferState::Playing { .. } => {
                    self.p2p_buffer_state = P2pBufferState::Idle;
                    self.player.current_remote = None;
                }
                _ => {
                    self.advance_track();
                }
            }
        }
        self.tick_transfer();
        self.tick_organize();
        self.tick_amazon_cdp();
        self.tick_p2p();
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

    pub fn tick_organize(&mut self) {
        while let Some(ev) = self.organize.poll_event() {
            let Some(state) = &mut self.organizer_state else { continue };
            match ev {
                OrganizerEvent::Started { title, index, total } => {
                    state.log.push(format!("[{}/{}] Moving: {}", index + 1, total, title));
                }
                OrganizerEvent::FileDone { title, dest } => {
                    state.log.push(format!("  ✓ {} → {}", title, dest));
                }
                OrganizerEvent::FileFailed { title, reason } => {
                    if title.is_empty() {
                        state.log.push(format!("  {reason}"));
                    } else {
                        state.log.push(format!("  ✗ {} — {}", title, reason));
                    }
                }
                OrganizerEvent::BatchComplete { succeeded, failed, dest_dir } => {
                    state.log.push(format!(
                        "Done. {succeeded} moved, {failed} failed."
                    ));
                    state.results = Some((succeeded, failed));
                    state.phase = OrganizerPhase::Done;
                    // Auto-scroll to bottom
                    state.log_scroll = state.log.len().saturating_sub(1);

                    // Add the destination directory as a new source and rescan.
                    if !self.source_dirs.contains(&dest_dir) {
                        self.source_dirs.push(dest_dir);
                        // Persist the updated source list.
                        {
                            let mut cfg = Config::load();
                            cfg.source_dirs = self.source_dirs.clone();
                            cfg.save();
                        }
                    }
                    self.rescan();
                }
            }
        }
    }

    // ---------------------------------------------------------------------------
    // P2P methods
    // ---------------------------------------------------------------------------

    /// Returns `true` if `s` is a valid P2P display name: 1–12 ASCII alphanumeric chars.
    pub fn is_valid_p2p_name(s: &str) -> bool {
        !s.is_empty() && s.len() <= 12 && s.chars().all(|c| c.is_ascii_alphanumeric())
    }

    /// Build the composite display name shown in the UI: `"nickname#ABCD"`.
    ///
    /// The four-character suffix is the last four hex digits of the PGP
    /// fingerprint, uppercased.  Two nodes with the same nickname will thus
    /// always show differently so the user can tell them apart.
    pub fn p2p_display_name(nickname: &str, fingerprint: &str) -> String {
        let tag = fingerprint
            .get(fingerprint.len().saturating_sub(4)..)
            .unwrap_or(fingerprint);
        format!("{}#{}", nickname, tag.to_uppercase())
    }

    /// Activate P2P mode (called on `p`→`2`→`p` chord completion).
    /// Loads or generates the PGP identity, spawns the libp2p node, and
    /// stores a `P2pHandle` in `self.p2p_node`.
    pub fn activate_p2p(&mut self) {
        if self.p2p_node.is_some() {
            // Already active — jump to peer management screen.
            self.screen = Screen::P2pPeers;
            return;
        }

        let mut cfg = Config::load();

        // If no valid nickname is stored, send the user to the identity screen
        // first.  activate_p2p() will be called again after they confirm a name.
        let nickname = match cfg.p2p_nickname.as_deref().filter(|n| Self::is_valid_p2p_name(n)) {
            Some(n) => n.to_string(),
            None => {
                self.screen = Screen::P2pIdentity;
                return;
            }
        };

        // Resolve passphrase: OS keychain is authoritative; config.json is the
        // migration fallback for existing installs that stored it there.
        let keychain_hit = crate::keychain::load_passphrase();
        let passphrase_from_config =
            keychain_hit.is_none() && cfg.p2p_identity_passphrase.is_some();
        let resolved_passphrase = keychain_hit.or_else(|| cfg.p2p_identity_passphrase.clone());

        match PgpIdentity::load_or_generate(
            &nickname,
            cfg.p2p_identity_armored.as_deref(),
            resolved_passphrase.as_deref(),
        ) {
            Ok((identity, new_keys)) => {
                if let Some((armored, passphrase)) = new_keys {
                    // Fresh keypair — try to store the passphrase in the OS
                    // keychain.  Fall back to config.json if unavailable.
                    cfg.p2p_identity_armored = Some(armored);
                    cfg.p2p_nickname         = Some(nickname.clone());
                    match crate::keychain::store_passphrase(&passphrase) {
                        crate::keychain::StoreOutcome::Keychain => {
                            cfg.p2p_identity_passphrase = None;
                            cfg.save();
                            self.push_toast(Toast::info(
                                "P2P identity generated — passphrase secured in OS keychain.",
                            ));
                        }
                        crate::keychain::StoreOutcome::ConfigFallback(reason) => {
                            cfg.p2p_identity_passphrase = Some(passphrase);
                            cfg.save();
                            self.push_toast(Toast::warning(format!(
                                "P2P identity generated — passphrase stored in config.json \
                                 (keychain unavailable: {reason})."
                            )));
                        }
                    }
                } else if passphrase_from_config {
                    // Migration: passphrase was already in config.json from a
                    // previous install.  Move it to the keychain now and scrub
                    // it from the file.
                    if let Some(ref pw) = resolved_passphrase {
                        match crate::keychain::store_passphrase(pw) {
                            crate::keychain::StoreOutcome::Keychain => {
                                cfg.p2p_identity_passphrase = None;
                                cfg.save();
                                self.push_toast(Toast::info(
                                    "P2P passphrase migrated to OS keychain \
                                     and removed from config.json.",
                                ));
                            }
                            crate::keychain::StoreOutcome::ConfigFallback(_) => {
                                // Keychain still unavailable — leave it in config.json
                                // and stay silent so we don't toast on every launch.
                            }
                        }
                    }
                }

                let fp = identity.fingerprint();

                // Build channel pair: UI-side handle + node-side channels
                let (handle, channels) = P2pHandle::channel(nickname.clone(), fp.clone());

                // Parse bootstrap peers from config multiaddrs.
                // Each string is expected to be a full /ip4/.../tcp/.../p2p/<PeerId> multiaddr.
                let bootstrap_peers: Vec<(libp2p::PeerId, libp2p::Multiaddr)> = cfg
                    .p2p_bootstrap_peers
                    .iter()
                    .filter_map(|addr_str| {
                        let ma: libp2p::Multiaddr = addr_str.parse().ok()?;
                        // Extract the P2p protocol component (last segment)
                        let peer_id = ma.iter().find_map(|p| {
                            if let libp2p::multiaddr::Protocol::P2p(hash) = p {
                                libp2p::PeerId::from_multihash(hash.into()).ok()
                            } else {
                                None
                            }
                        })?;
                        Some((peer_id, ma))
                    })
                    .collect();

                // Apply live-reloadable config values.
                self.p2p_stall_secs   = cfg.p2p_stall_secs;
                self.p2p_abandon_secs = cfg.p2p_abandon_secs;

                match MusicNode::spawn(
                    identity,
                    bootstrap_peers,
                    cfg.p2p_listen_port,
                    cfg.p2p_chunk_retries,
                    cfg.p2p_stall_secs,
                    cfg.p2p_abandon_secs,
                    channels.cmd_rx,
                    channels.event_tx,
                ) {
                    Ok(()) => {
                        // Broadcast library catalog immediately after activation
                        let catalog = crate::p2p::catalog::build_catalog(&self.library);
                        handle.send(crate::p2p::P2pCommand::AnnounceLibrary(catalog));
                        let paths = crate::p2p::catalog::build_path_map(&self.library);
                        handle.send(crate::p2p::P2pCommand::SetLocalPaths(paths));
                        self.p2p_node = Some(handle);
                        self.screen = Screen::P2pPeers;
                        let display = Self::p2p_display_name(&nickname, &fp);
                        self.status_message = Some(format!(
                            "⬡ P2P active — {display}  Discovering peers…"
                        ));
                    }
                    Err(e) => {
                        self.push_toast(Toast::error(format!("P2P node start failed: {e}")));
                    }
                }
            }
            Err(e) => {
                self.push_toast(Toast::error(format!("P2P identity error: {e}")));
            }
        }
    }

    /// Deactivate P2P mode, disconnecting from the network.
    pub fn deactivate_p2p(&mut self) {
        if let Some(node) = &self.p2p_node {
            node.send(crate::p2p::P2pCommand::Disconnect);
        }
        self.p2p_node = None;
        self.p2p_buffer_state = P2pBufferState::Idle;
        self.remote_tracks.clear();
        self.p2p_peer_list.clear();
        self.p2p_listen_addrs.clear();
        self.screen = Screen::Library;
        self.status_message = Some("P2P disconnected.".into());
    }

    /// Open the Settings screen, populating fields from `cfg`.
    pub fn open_settings(&mut self, cfg: &crate::config::Config) {
        self.settings_fields = vec![
            SettingsField {
                label: "Chunk retries",
                description: "How many times a receiver retries missing chunks before giving up (default: 5)",
                value: cfg.p2p_chunk_retries.to_string(),
                key: "p2p_chunk_retries",
            },
            SettingsField {
                label: "Beacon interval (s)",
                description: "LAN beacon broadcast interval in seconds — lower = faster discovery (default: 2)",
                value: cfg.p2p_beacon_interval_secs.to_string(),
                key: "p2p_beacon_interval_secs",
            },
            SettingsField {
                label: "mDNS interval (s)",
                description: "mDNS probe interval in seconds (default: 30)",
                value: cfg.p2p_mdns_interval_secs.to_string(),
                key: "p2p_mdns_interval_secs",
            },
            SettingsField {
                label: "Stall threshold (s)",
                description: "Seconds without a chunk before a transfer is flagged as stalled (default: 5)",
                value: cfg.p2p_stall_secs.to_string(),
                key: "p2p_stall_secs",
            },
            SettingsField {
                label: "Abandon timeout (s)",
                description: "Seconds after stall before a transfer is abandoned entirely (default: 30)",
                value: cfg.p2p_abandon_secs.to_string(),
                key: "p2p_abandon_secs",
            },
            SettingsField {
                label: "P2P listen port",
                description: "Fixed TCP/UDP port for P2P (0 = random). Requires restart to take effect.",
                value: cfg.p2p_listen_port.unwrap_or(0).to_string(),
                key: "p2p_listen_port",
            },
        ];
        self.settings_selected = 0;
        self.screen = Screen::Settings;
    }

    /// Parse settings fields back into `cfg`, save to disk, and hot-reload
    /// values that can be applied without a restart.
    /// Returns a human-readable summary of what changed.
    pub fn apply_settings(&mut self, cfg: &mut crate::config::Config) -> String {
        let mut changed: Vec<String> = Vec::new();

        for field in &self.settings_fields {
            match field.key {
                "p2p_chunk_retries" => {
                    if let Ok(v) = field.value.trim().parse::<u32>() {
                        let v = v.max(1);
                        if cfg.p2p_chunk_retries != v {
                            cfg.p2p_chunk_retries = v;
                            changed.push(format!("chunk retries → {v}"));
                        }
                    }
                }
                "p2p_beacon_interval_secs" => {
                    if let Ok(v) = field.value.trim().parse::<u64>() {
                        let v = v.max(1);
                        if cfg.p2p_beacon_interval_secs != v {
                            cfg.p2p_beacon_interval_secs = v;
                            changed.push(format!("beacon interval → {v}s"));
                        }
                    }
                }
                "p2p_mdns_interval_secs" => {
                    if let Ok(v) = field.value.trim().parse::<u64>() {
                        let v = v.max(1);
                        if cfg.p2p_mdns_interval_secs != v {
                            cfg.p2p_mdns_interval_secs = v;
                            changed.push(format!("mDNS interval → {v}s"));
                        }
                    }
                }
                "p2p_stall_secs" => {
                    if let Ok(v) = field.value.trim().parse::<u64>() {
                        let v = v.max(1);
                        if cfg.p2p_stall_secs != v {
                            cfg.p2p_stall_secs = v;
                            self.p2p_stall_secs = v;  // hot-reload
                            changed.push(format!("stall threshold → {v}s"));
                        }
                    }
                }
                "p2p_abandon_secs" => {
                    if let Ok(v) = field.value.trim().parse::<u64>() {
                        let v = v.max(1);
                        if cfg.p2p_abandon_secs != v {
                            cfg.p2p_abandon_secs = v;
                            self.p2p_abandon_secs = v;  // hot-reload
                            changed.push(format!("abandon timeout → {v}s"));
                        }
                    }
                }
                "p2p_listen_port" => {
                    if let Ok(v) = field.value.trim().parse::<u16>() {
                        let new_port = if v == 0 { None } else { Some(v) };
                        if cfg.p2p_listen_port != new_port {
                            cfg.p2p_listen_port = new_port;
                            changed.push(format!("listen port → {} (restart required)", v));
                        }
                    }
                }
                _ => {}
            }
        }

        cfg.save();

        if changed.is_empty() {
            "Settings unchanged.".into()
        } else {
            format!("Saved: {}", changed.join(", "))
        }
    }

    /// Push a toast notification onto the queue.
    pub fn push_toast(&mut self, toast: Toast) {
        // Keep at most 5 toasts — drop oldest if full.
        if self.p2p_toasts.len() >= 5 {
            self.p2p_toasts.pop_front();
        }
        self.p2p_toasts.push_back(toast);
    }

    /// Drain expired (non-error) toasts from the queue.  Called every tick.
    pub fn prune_toasts(&mut self) {
        self.p2p_toasts.retain(|t| !t.is_expired());
    }

    /// Dismiss the topmost error toast (called on `Esc` from any P2P screen).
    pub fn dismiss_error_toast(&mut self) {
        if let Some(pos) = self
            .p2p_toasts
            .iter()
            .rposition(|t| t.level == ToastLevel::Error)
        {
            self.p2p_toasts.remove(pos);
        }
    }

    /// Poll P2P events and update app state accordingly.  Called every tick.
    pub fn tick_p2p(&mut self) {
        // Prune expired toasts every tick.
        self.prune_toasts();

        // Prune expired party nominations.
        if let Some(party) = &mut self.party_line {
            party.prune_expired();
        }

        // Stall detection for in-progress buffer.
        let stall_secs   = self.p2p_stall_secs;
        let abandon_secs = self.p2p_abandon_secs;
        if let P2pBufferState::Buffering { stalled, stalled_since, last_chunk_at, transfer_id, peer_nick, .. } =
            &mut self.p2p_buffer_state
        {
            let elapsed = last_chunk_at.elapsed().as_secs();
            if elapsed >= stall_secs && !*stalled {
                *stalled = true;
                *stalled_since = Some(std::time::Instant::now());
            }
            if let Some(since) = stalled_since {
                if since.elapsed().as_secs() >= abandon_secs {
                    let tid = *transfer_id;
                    let nick = peer_nick.clone();
                    self.p2p_buffer_state = P2pBufferState::Idle;
                    self.push_toast(Toast::error(format!(
                        "Transfer timed out — {nick} may be offline"
                    )));
                    if let Some(node) = &self.p2p_node {
                        node.send(crate::p2p::P2pCommand::DeclineTrackRequest { transfer_id: tid });
                    }
                    return;
                }
            }
        }

        // Party line: resume playback when start_at arrives.
        let should_resume = self.party_line.as_ref().and_then(|p| p.active.as_ref()).map(|a| {
            a.buffer_ready && !a.started && chrono::Utc::now() >= a.start_at
        }).unwrap_or(false);
        if should_resume {
            self.player.toggle_pause(); // un-pause (was paused awaiting start_at)
            if let Some(party) = &mut self.party_line {
                if let Some(active) = &mut party.active {
                    active.started = true;
                }
            }
            self.push_toast(Toast::info("Party Line started!"));
        }

        // Drain P2P events from the background node.
        let events = self.p2p_node.as_mut().map(|n| n.poll()).unwrap_or_default();
        for event in events {
            self.handle_p2p_event(event);
        }
    }

    fn handle_p2p_event(&mut self, event: crate::p2p::P2pEvent) {
        use crate::p2p::P2pEvent;
        use crate::p2p::trust::{NodeInfo, NodeStatus, TrustState};
        match event {
            P2pEvent::PeerApprovalRequired { fingerprint, nickname } => {
                // Filter self-connections (belt-and-suspenders; node also filters).
                if self.p2p_node.as_ref().map(|n| n.fingerprint == fingerprint).unwrap_or(false) {
                    return;
                }
                let display = Self::p2p_display_name(&nickname, &fingerprint);
                self.push_toast(Toast::info(format!("New peer: {display} — press A to approve")));
                // Add to the peer list immediately so the user can act on it.
                if !self.p2p_peer_list.iter().any(|p| p.fingerprint == fingerprint) {
                    self.p2p_peer_list.push(NodeInfo {
                        fingerprint,
                        nickname,
                        trust: TrustState::Pending,
                        status: NodeStatus::Online,
                        last_seen: chrono::Utc::now(),
                    });
                }
            }
            P2pEvent::PeerTrusted { fingerprint, nickname } => {
                let display = Self::p2p_display_name(&nickname, &fingerprint);
                self.push_toast(Toast::info(format!("{display} trusted")));
                if let Some(p) = self.p2p_peer_list.iter_mut().find(|p| p.fingerprint == fingerprint) {
                    p.trust = TrustState::Trusted;
                }
            }
            P2pEvent::PeerOffline { fingerprint, nickname } => {
                let display = Self::p2p_display_name(&nickname, &fingerprint);
                self.push_toast(Toast::warning(format!("{display} went offline")));
                if let Some(p) = self.p2p_peer_list.iter_mut().find(|p| p.fingerprint == fingerprint) {
                    p.status = NodeStatus::Offline;
                }
            }
            P2pEvent::PeerListSnapshot(peers) => {
                self.p2p_peer_list = peers;
            }
            P2pEvent::RemoteCatalogReceived { peer_fp, peer_nick, mut tracks } => {
                // Tag each track with its owner.
                for t in &mut tracks {
                    t.owner_fp   = peer_fp.clone();
                    t.owner_nick = peer_nick.clone();
                }
                // Merge into remote_tracks, replacing any previous entries from this peer.
                self.remote_tracks.retain(|t| t.owner_fp != peer_fp);
                self.remote_tracks.extend(tracks);
                // The node already emits a "catalog complete" Info toast; no
                // duplicate toast needed here.
            }
            P2pEvent::InboundTrackRequest { transfer_id, track, requester_fp: _ } => {
                // Auto-accept for now; Phase 5 will add a confirmation prompt.
                if let Some(node) = &self.p2p_node {
                    node.send(crate::p2p::P2pCommand::AcceptTrackRequest { transfer_id });
                }
                let _ = track;
            }
            P2pEvent::TrackBufferProgress { transfer_id, received, total, track } => {
                match &self.p2p_buffer_state {
                    // TrackOffer arrived — transition Requesting → Buffering.
                    P2pBufferState::Requesting { peer_nick, .. } => {
                        let peer_nick     = peer_nick.clone();
                        let track_title   = track.as_ref().map(|t| t.title.clone()).unwrap_or_default();
                        let track_artist  = track.as_ref().map(|t| t.artist.clone()).unwrap_or_default();
                        self.p2p_buffer_state = P2pBufferState::Buffering {
                            transfer_id,
                            peer_nick,
                            track_title,
                            track_artist,
                            received,
                            total,
                            stalled: false,
                            stalled_since: None,
                            last_chunk_at: std::time::Instant::now(),
                        };
                    }
                    // Subsequent chunk progress — update in place.
                    P2pBufferState::Buffering { transfer_id: tid, .. }
                        if *tid == transfer_id =>
                    {
                        if let P2pBufferState::Buffering {
                            received: r, total: t, stalled, last_chunk_at, ..
                        } = &mut self.p2p_buffer_state {
                            *r = received;
                            *t = total;
                            *stalled = false;
                            *last_chunk_at = std::time::Instant::now();
                        }
                    }
                    _ => {}
                }
            }
            P2pEvent::TrackBufferReady { bytes, track, transfer_id } => {
                let nick = if let P2pBufferState::Buffering { ref peer_nick, .. } = self.p2p_buffer_state {
                    peer_nick.clone()
                } else {
                    track.owner_nick.clone()
                };

                // Check if this is for an active party line — if so, defer playback to start_at.
                let is_party = self.party_line
                    .as_ref()
                    .and_then(|p| p.active.as_ref())
                    .map(|a| a.track.id == track.id && !a.started)
                    .unwrap_or(false);

                if is_party {
                    // Mark buffer ready; tick_p2p will start playback at start_at.
                    if let Some(party) = &mut self.party_line {
                        if let Some(active) = &mut party.active {
                            active.buffer_ready = true;
                        }
                    }
                    // Hold bytes in a temporary field via a side-channel: store in player now,
                    // but pause immediately until start_at arrives.
                    match self.player.play_remote(bytes, track) {
                        Ok(()) => {
                            self.player.toggle_pause(); // pause immediately
                            self.p2p_buffer_state = P2pBufferState::Playing { peer_nick: nick };
                        }
                        Err(e) => {
                            self.p2p_buffer_state = P2pBufferState::Idle;
                            self.push_toast(Toast::error(format!("Party playback error: {e}")));
                        }
                    }
                    let _ = transfer_id;
                } else {
                    match self.player.play_remote(bytes, track) {
                        Ok(()) => {
                            self.p2p_buffer_state = P2pBufferState::Playing { peer_nick: nick };
                        }
                        Err(e) => {
                            self.p2p_buffer_state = P2pBufferState::Idle;
                            self.push_toast(Toast::error(format!("Playback error: {e}")));
                        }
                    }
                }
            }
            P2pEvent::TrackTransferFailed { reason, .. } => {
                self.p2p_buffer_state = P2pBufferState::Idle;
                self.push_toast(Toast::error(format!("Transfer failed: {reason}")));
            }
            P2pEvent::TrackNominated { nomination_id, track, nominated_by } => {
                let party = self.party_line.get_or_insert_with(PartyLineState::new);
                party.upsert_nomination(crate::p2p::party::Nomination::new(
                    nomination_id, track.clone(), nominated_by.clone(),
                ));
                self.push_toast(Toast::info(format!(
                    "{nominated_by} nominated: {} — vote in Party Line", track.title
                )));
            }
            P2pEvent::VoteReceived { nomination_id, voter_fp, vote } => {
                if let Some(party) = &mut self.party_line {
                    if let Some(nom) = party.nominations.iter_mut().find(|n| n.id == nomination_id) {
                        nom.cast_vote(&voter_fp, &vote);
                    }
                }
            }
            P2pEvent::PartyLinePassed { nomination_id, track, start_at } => {
                // If the track is from a remote peer, request it immediately so
                // the buffer is ready before start_at.
                if !track.owner_fp.is_empty() {
                    if let Some(node) = &self.p2p_node {
                        let peer_fp = track.owner_fp.clone();
                        let peer_nick = track.owner_nick.clone();
                        self.p2p_buffer_state = crate::p2p::P2pBufferState::Requesting {
                            track_id: track.id,
                            peer_nick,
                        };
                        node.send(crate::p2p::P2pCommand::RequestTrack {
                            track_id: track.id,
                            peer_fp,
                        });
                    }
                }
                let party = self.party_line.get_or_insert_with(PartyLineState::new);
                party.active = Some(crate::p2p::party::ActiveParty {
                    nomination_id,
                    track: track.clone(),
                    start_at,
                    buffer_ready: false,
                    started: false,
                });
                self.push_toast(Toast::info(format!(
                    "Party Line: {} — starts in 5s", track.title
                )));
            }
            P2pEvent::PartyLineFailed { .. } => {
                self.push_toast(Toast::warning("Party Line vote expired."));
            }
            P2pEvent::Info(msg) => {
                self.push_toast(Toast::info(msg));
            }
            P2pEvent::Warning(msg) => {
                self.push_toast(Toast::warning(msg));
            }
            P2pEvent::ListenAddrsUpdated(addrs) => {
                self.p2p_listen_addrs = addrs;
            }
        }
    }

    // ---------------------------------------------------------------------------
    // Organizer methods
    // ---------------------------------------------------------------------------

    /// Open the organizer screen.  Builds groups from:
    /// 1. "Current selection" if any tracks are selected (always first)
    /// 2. Groups derived from the current library sort order (if GroupBy*)
    /// 3. Always: groups by Artist, Album, Year as fallback options
    pub fn begin_organize(&mut self) {
        let mut groups: Vec<OrganizerGroup> = Vec::new();

        // ── 1. Current selection ──────────────────────────────────────────
        if !self.selected_tracks.is_empty() {
            let tracks: Vec<_> = self.selected_tracks
                .iter()
                .filter_map(|&i| self.library.tracks.get(i).cloned())
                .collect();
            if !tracks.is_empty() {
                groups.push(OrganizerGroup {
                    label: format!("Current selection ({} tracks)", tracks.len()),
                    tracks,
                });
            }
        }

        // ── 2. Current sort-order groups (if GroupBy*) ────────────────────
        if self.library.sort_order.has_sections() {
            let mut seen: Vec<String> = Vec::new();
            for track in &self.library.tracks {
                let key = self.library.section_key(track)
                    .unwrap_or_else(|| "Other".into());
                if !seen.contains(&key) {
                    seen.push(key.clone());
                }
            }
            for key in seen {
                let k = key.clone();
                let tracks: Vec<_> = self.library.tracks
                    .iter()
                    .filter(|t| {
                        self.library.section_key(t).as_deref() == Some(k.as_str())
                    })
                    .cloned()
                    .collect();
                if !tracks.is_empty() {
                    let label = format!("{} — {} ({} tracks)",
                        self.library.sort_order.label(), key, tracks.len());
                    groups.push(OrganizerGroup { label, tracks });
                }
            }
        }

        // ── 3. Always offer Artist / Year / Extension groups ─────────────
        use crate::library::SortOrder;
        for order in &[SortOrder::GroupByArtist, SortOrder::GroupByYear, SortOrder::GroupByExtension] {
            let mut seen: Vec<String> = Vec::new();
            for track in &self.library.tracks {
                let key = order.section_key(track).unwrap_or_else(|| "Other".into());
                if !seen.contains(&key) {
                    seen.push(key.clone());
                }
            }
            for key in seen {
                // Skip duplicates that already appeared from the current sort-order pass
                let label = format!("{} — {} ({} tracks)",
                    order.label(), key,
                    self.library.tracks.iter().filter(|t| {
                        order.section_key(t).as_deref() == Some(key.as_str())
                    }).count());
                if !groups.iter().any(|g| g.label == label) {
                    let tracks: Vec<_> = self.library.tracks
                        .iter()
                        .filter(|t| order.section_key(t).as_deref() == Some(key.as_str()))
                        .cloned()
                        .collect();
                    if !tracks.is_empty() {
                        groups.push(OrganizerGroup { label, tracks });
                    }
                }
            }
        }

        if groups.is_empty() {
            self.status_message = Some("No tracks or groups to organize.".into());
            return;
        }

        self.organizer_state = Some(OrganizerState::new(groups));
        self.screen = Screen::Organize;
    }

    /// Called when the user confirms the destination path and starts the move.
    pub fn confirm_organize_dest(&mut self) {
        let Some(state) = &mut self.organizer_state else { return };
        let dest_raw = state.dest_input.trim().to_string();
        if dest_raw.is_empty() {
            return;
        }
        let dest_dir = crate::util::expand_tilde(&dest_raw);
        let tracks = state.selected_group()
            .map(|g| g.tracks.clone())
            .unwrap_or_default();
        if tracks.is_empty() {
            return;
        }
        state.phase = OrganizerPhase::Running;
        state.log.clear();
        state.log_scroll = 0;
        self.organize.start_batch(tracks, dest_dir);
    }
}
