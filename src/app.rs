//! Top-level application state.

use std::path::PathBuf;
use std::sync::Arc;

use rodio::OutputStreamHandle;

use ipod_rs::{DeviceScanResult, DeviceTrackEntry};

use crate::device::ipod_ums::IpodUmsDevice;
use crate::device::MusicDevice;
use crate::library::scanner;
use crate::library::{Library, TrackEdit};
use crate::player::Player;
use crate::playlist::{ConflictCtx, Playlist};
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

    // --- marquee scrolling ---
    /// Monotonically incrementing tick counter, reset whenever the focused
    /// library track changes.  The UI computes the scroll offset from this.
    pub marquee_tick: u32,
}

impl App {
    pub fn new(source_dirs: Vec<PathBuf>, audio_handle: Option<OutputStreamHandle>) -> Self {
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
            marquee_tick: 0,
        };
        app.rescan();
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
            }
            Err(e) => self.status_message = Some(format!("Scan error: {e}")),
        }
    }

    // --- source dirs ---

    pub fn add_source(&mut self, path: PathBuf) {
        if !path.is_dir() {
            self.status_message = Some(format!("Not a directory: {}", path.display()));
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
            self.player.play(&track);
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

    // --- tick ---

    pub fn tick(&mut self) {
        self.player.tick();
        self.tick_transfer();
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
