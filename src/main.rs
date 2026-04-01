//! console-music-player — entry point.
#![allow(dead_code)]

mod amazon;
mod app;
mod config;
mod device;
mod error;
mod library;
mod media;
mod player;
mod playlist;
mod tags;
mod tracker;
mod transfer;
mod ui;
mod visualizer;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use tracing::info;
use tracing_subscriber::EnvFilter;

use app::{App, Focus, Screen};
use config::Config;

// ---------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tracker DLL probe (Windows + MSVC + tracker feature only)
// ---------------------------------------------------------------------------
// openmpt.dll is delay-loaded (see build.rs), so the process starts even when
// the DLL is absent. We probe here in main() and exit cleanly with install
// instructions instead of crashing on the first tracker call.

#[cfg(all(feature = "tracker", target_os = "windows"))]
fn check_openmpt_dll() {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    extern "system" {
        fn LoadLibraryW(lpLibFileName: *const u16) -> *mut std::ffi::c_void;
        fn FreeLibrary(hModule: *mut std::ffi::c_void) -> i32;
    }

    let name: Vec<u16> = OsStr::new("libopenmpt.dll")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let h = unsafe { LoadLibraryW(name.as_ptr()) };
    if h.is_null() {
        eprintln!(
            "\nconsole-music-player: tracker feature is enabled but \
             libopenmpt.dll was not found.\n\
             \n\
             To fix this, copy libopenmpt.dll into the same directory as cmp.exe:\n\
             \n\
             {}\n\
             \n\
             Download the Windows package from https://lib.openmpt.org/libopenmpt/download/\n\
             or see README.md for full instructions.\n\
             \n\
             To run without tracker support, build with:\n\
               cargo run --no-default-features",
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.display().to_string()))
                .unwrap_or_else(|| "<exe directory>".into())
        );
        std::process::exit(1);
    }
    unsafe { FreeLibrary(h) };
}

fn parse_initial_library_arg() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1);
    loop {
        match args.next().as_deref() {
            Some("--library") | Some("-l") => return args.next().map(PathBuf::from),
            Some(p) if !p.starts_with('-') => return Some(PathBuf::from(p)),
            None => return None,
            _ => {}
        }
    }
}

fn default_music_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(PathBuf::from(home).join("Music"))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let mut cfg = Config::load();

    if let Some(p) = parse_initial_library_arg() {
        if !cfg.source_dirs.contains(&p) {
            cfg.source_dirs.push(p);
        }
    }
    if cfg.source_dirs.is_empty() {
        if let Some(d) = default_music_dir() {
            if d.is_dir() {
                info!("Seeding default source: {}", d.display());
                cfg.source_dirs.push(d);
            }
        }
    }
    cfg.save();

    // On Windows, libopenmpt.dll is a load-time dependency when the tracker
    // feature is enabled. If the DLL is absent the OS loader will abort the
    // process before reaching main(), so we probe for it here first and give
    // the user an actionable message rather than a cryptic crash.
    #[cfg(all(feature = "tracker", target_os = "windows"))]
    check_openmpt_dll();

    // On Android/Termux, cpal calls ndk-context to obtain the Java AudioManager.
    // No JavaVM exists in a plain terminal process, so the call panics instead of
    // returning Err.  catch_unwind turns that panic into a None so the app starts
    // cleanly without audio rather than crashing.
    let _audio_stream = std::panic::catch_unwind(rodio::OutputStream::try_default)
        .ok()
        .and_then(|r| r.ok());
    let audio_handle = _audio_stream.as_ref().map(|(_, h)| h.clone());
    if audio_handle.is_none() {
        eprintln!("Warning: no audio output device — playback unavailable.");
    }

    let mut app = App::new(
        cfg.source_dirs.clone(),
        audio_handle,
        cfg.amazon_cookie.clone(),
        cfg.amazon_download_dir.clone(),
    );
    refresh_devices(&mut app);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut app, &mut terminal, &mut cfg).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

// ---------------------------------------------------------------------------
// Event loop
// ---------------------------------------------------------------------------

async fn run_event_loop(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cfg: &mut Config,
) -> anyhow::Result<()> {
    const TICK: Duration = Duration::from_millis(100);
    while app.running {
        terminal.draw(|frame| ui::render(app, frame))?;
        app.tick();

        // Spawn an Amazon catalog fetch when amazon_state.needs_fetch is set.
        // We clear the flag immediately so we only spawn once per request.
        if let Some(state) = &mut app.amazon_state {
            if state.needs_fetch && state.overlay.is_none() {
                state.needs_fetch = false;
                if let Some(cookie) = &app.amazon_cookie {
                    let client = amazon::AmazonClient::new(cookie.clone());
                    let inbox = app.amazon_inbox.clone();
                    tokio::spawn(async move { client.fetch_catalog(inbox).await });
                }
            }
        }

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                // Ctrl+V — paste clipboard text into whichever input field is active.
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('v')
                {
                    let text = clipboard_paste().unwrap_or_default();
                    let clean: String = text.chars().filter(|c| !c.is_control() || *c == ' ').collect();
                    if !clean.is_empty() {
                        // Tag edit overlay has its own input field.
                        if let Some(state) = &mut app.tag_edit_state {
                            state.input.push_str(&clean);
                        } else {
                            app.input_buffer.push_str(&clean);
                        }
                    }
                    continue;
                }

                // Don't wipe status while the user is typing
                let is_input_screen = matches!(
                    app.screen,
                    Screen::AddSource | Screen::SavePlaylist | Screen::EditTrack | Screen::Amazon
                );
                if !is_input_screen {
                    app.status_message = None;
                }
                handle_key(app, key.code, cfg);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Key dispatch
// ---------------------------------------------------------------------------

/// Number of items to jump on PageUp / PageDown.
const PAGE_SIZE: usize = 10;

fn handle_key(app: &mut App, key: KeyCode, cfg: &mut Config) {
    // Tag edit overlay intercepts all keys.
    if app.tag_edit_state.is_some() {
        handle_tag_edit_key(app, key);
        return;
    }

    match app.screen {
        Screen::Library         => handle_library_key(app, key),
        Screen::EditTrack       => handle_edit_key(app, key),
        Screen::Dedup           => handle_dedup_key(app, key),
        Screen::Sources         => handle_sources_key(app, key),
        Screen::AddSource       => handle_text_input_key(app, key, |app| {
            let path = PathBuf::from(app.input_buffer.trim());
            app.input_buffer.clear();
            app.screen = Screen::Sources;
            app.add_source(path);
            persist_sources(app);
        }),
        Screen::Playlists       => handle_playlists_key(app, key),
        Screen::SavePlaylist    => handle_text_input_key(app, key, |app| {
            app.confirm_save_playlist();
        }),
        Screen::PlaylistConflict => handle_conflict_key(app, key),
        Screen::Transfer        => handle_transfer_key(app, key),
        Screen::RepairIpod      => handle_repair_key(app, key),
        Screen::DeviceTracks    => handle_device_tracks_key(app, key),
        Screen::Amazon          => handle_amazon_key(app, key, cfg),
    }
}

/// Advance the A→C→E easter-egg key sequence.
/// Returns `true` if the key was consumed by the sequence tracker.
fn advance_amazon_seq(app: &mut App, c: char) -> bool {
    const SEQ: [char; 3] = ['a', 'c', 'e'];
    let lc = c.to_ascii_lowercase();
    let seq_len = app.amazon_key_seq.len();

    // Expire a stale partial sequence after 2 seconds.
    if seq_len > 0 {
        let expired = app
            .amazon_key_seq_time
            .map(|t| t.elapsed().as_secs() >= 2)
            .unwrap_or(true);
        if expired {
            app.amazon_key_seq.clear();
            app.amazon_key_seq_time = None;
        }
    }

    let seq_len = app.amazon_key_seq.len();

    if seq_len < SEQ.len() && lc == SEQ[seq_len] {
        if seq_len == 0 {
            app.amazon_key_seq_time = Some(std::time::Instant::now());
        }
        app.amazon_key_seq.push(lc);
        if app.amazon_key_seq.len() == SEQ.len() {
            // Complete! Activate the easter egg.
            app.amazon_key_seq.clear();
            app.amazon_key_seq_time = None;
            app.activate_amazon();
        }
        return true; // key consumed
    }

    // Wrong key — reset any in-progress sequence (but don't consume the key).
    if seq_len > 0 {
        app.amazon_key_seq.clear();
        app.amazon_key_seq_time = None;
    }
    false
}

fn handle_library_key(app: &mut App, key: KeyCode) {
    // Esc closes the waveform overlay if it's active.
    if key == KeyCode::Esc && app.waveform_active {
        app.waveform_active = false;
        return;
    }

    // Easter egg: A→C→E rapid sequence opens the Amazon Music screen.
    // 'A' and 'C' are unbound in the library, so consuming them is safe.
    // 'E' normally opens the tag editor — it's only consumed when completing
    // the sequence (i.e. 'A' and 'C' were already pressed within 2 seconds).
    if let KeyCode::Char(c) = key {
        if advance_amazon_seq(app, c) {
            return;
        }
    }

    match key {
        KeyCode::Char('q') | KeyCode::Char('Q') => app.running = false,

        KeyCode::Tab => {
            app.focus = match app.focus {
                Focus::Library => Focus::Devices,
                Focus::Devices => Focus::Library,
            };
        }

        KeyCode::Up | KeyCode::Char('k') => match app.focus {
            Focus::Library => { app.library.move_up(); app.reset_marquee(); }
            Focus::Devices => app.move_device_up(),
        },
        KeyCode::Down | KeyCode::Char('j') => match app.focus {
            Focus::Library => { app.library.move_down(); app.reset_marquee(); }
            Focus::Devices => app.move_device_down(),
        },
        KeyCode::PageUp => {
            if app.focus == Focus::Library { app.library.page_up(PAGE_SIZE); app.reset_marquee(); }
        }
        KeyCode::PageDown => {
            if app.focus == Focus::Library { app.library.page_down(PAGE_SIZE); app.reset_marquee(); }
        }

        KeyCode::Char(' ') => {
            if app.focus == Focus::Library {
                app.toggle_selected_track();
            }
        }

        // Playback
        KeyCode::Enter          => app.play_focused(),
        KeyCode::Char('p') | KeyCode::Char('P') => app.player.toggle_pause(),
        KeyCode::Char(']')      => app.player.volume_up(),
        KeyCode::Char('[')      => app.player.volume_down(),

        // Screens
        KeyCode::Char('s') | KeyCode::Char('S') => app.screen = Screen::Sources,
        KeyCode::Char('l') | KeyCode::Char('L') => {
            app.refresh_playlist_names();
            app.screen = Screen::Playlists;
        }
        KeyCode::Char('w') | KeyCode::Char('W') => app.begin_save_playlist(),

        // Library ops
        KeyCode::Char('t') | KeyCode::Char('T') => app.start_transfer(),
        KeyCode::Char('r') | KeyCode::Char('R') => {
            // R clears playlist filter if one is active, otherwise rescans
            if app.library.active_playlist.is_some() {
                app.library.clear_playlist();
                app.status_message = Some("Main Media Library.".into());
            } else {
                app.rescan();
            }
        }
        KeyCode::Char('d') | KeyCode::Char('D') => refresh_devices(app),
        KeyCode::Char('x') | KeyCode::Char('X') => app.scan_device_health(),
        KeyCode::Char('i') | KeyCode::Char('I') => app.load_device_tracks(),
        KeyCode::Char('n') | KeyCode::Char('N') => app.init_device_database(),
        KeyCode::Char('u') | KeyCode::Char('U') => app.dump_device_db(),
        KeyCode::Char('e') | KeyCode::Char('E') => {
            if app.focus == Focus::Library {
                app.begin_edit();
            }
        }
        KeyCode::Char('f') | KeyCode::Char('F') => app.begin_dedup(),
        KeyCode::Char('g') | KeyCode::Char('G') => app.begin_tag_edit(),
        KeyCode::Char('v') | KeyCode::Char('V') => {
            app.waveform_active = !app.waveform_active;
        }
        KeyCode::Char('o') | KeyCode::Char('O') => {
            app.player.toggle_repeat();
            let label = match app.player.repeat {
                crate::player::RepeatMode::Off => "Repeat off.",
                crate::player::RepeatMode::One => "Repeat one — track will loop.",
            };
            app.status_message = Some(label.into());
        }
        KeyCode::Char('z') | KeyCode::Char('Z') => {
            app.library.cycle_sort();
            app.status_message = Some(format!(
                "Sort: {}",
                app.library.sort_order.label()
            ));
        }

        _ => {}
    }
}

fn handle_dedup_key(app: &mut App, key: KeyCode) {
    use crate::app::DedupFocus;
    match key {
        KeyCode::Esc => app.cancel_dedup(),
        KeyCode::Enter => app.apply_dedup(),

        KeyCode::Tab => {
            if let Some(state) = &mut app.dedup_state {
                state.focus = match state.focus {
                    DedupFocus::Groups     => DedupFocus::Candidates,
                    DedupFocus::Candidates => DedupFocus::Groups,
                };
            }
        }

        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(state) = &mut app.dedup_state {
                match state.focus {
                    DedupFocus::Groups     => state.move_group_up(),
                    DedupFocus::Candidates => state.move_candidate_up(),
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(state) = &mut app.dedup_state {
                match state.focus {
                    DedupFocus::Groups     => state.move_group_down(),
                    DedupFocus::Candidates => state.move_candidate_down(),
                }
            }
        }
        KeyCode::PageUp => {
            if let Some(state) = &mut app.dedup_state {
                for _ in 0..PAGE_SIZE {
                    match state.focus {
                        DedupFocus::Groups     => state.move_group_up(),
                        DedupFocus::Candidates => state.move_candidate_up(),
                    }
                }
            }
        }
        KeyCode::PageDown => {
            if let Some(state) = &mut app.dedup_state {
                for _ in 0..PAGE_SIZE {
                    match state.focus {
                        DedupFocus::Groups     => state.move_group_down(),
                        DedupFocus::Candidates => state.move_candidate_down(),
                    }
                }
            }
        }

        // Cycle action for the focused candidate
        KeyCode::Char(' ') => {
            if let Some(state) = &mut app.dedup_state {
                if state.focus == DedupFocus::Candidates {
                    state.toggle_focused_action();
                }
            }
        }

        // Auto-suggest across all groups
        KeyCode::Char('a') | KeyCode::Char('A') => {
            if let Some(state) = &mut app.dedup_state {
                state.auto_suggest_all();
                app.status_message = Some("Auto-suggested actions applied.".into());
            }
        }

        _ => {}
    }
}

fn handle_edit_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc            => app.cancel_edit(),
        KeyCode::Enter          => app.confirm_edit(),
        KeyCode::Tab
        | KeyCode::Down
        | KeyCode::Char('j')    => app.edit_next_field(),
        KeyCode::Up
        | KeyCode::Char('k')    => app.edit_prev_field(),
        KeyCode::Backspace      => app.edit_backspace(),
        KeyCode::Char(c)        => app.edit_type_char(c),
        _ => {}
    }
}

fn handle_device_tracks_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Library,
        KeyCode::Up   | KeyCode::Char('k') => app.device_tracks_move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.device_tracks_move_down(),
        KeyCode::PageUp   => app.device_tracks_page_up(PAGE_SIZE),
        KeyCode::PageDown => app.device_tracks_page_down(PAGE_SIZE),
        _ => {}
    }
}

fn handle_repair_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc => {
            app.repair_results = None;
            app.screen = Screen::Library;
        }
        KeyCode::Char('f') | KeyCode::Char('F') => app.repair_all_issues(),
        KeyCode::Up | KeyCode::Char('k')  => app.repair_move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.repair_move_down(),
        KeyCode::PageUp   => app.repair_page_up(PAGE_SIZE),
        KeyCode::PageDown => app.repair_page_down(PAGE_SIZE),
        _ => {}
    }
}

fn handle_sources_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Library,
        KeyCode::Up | KeyCode::Char('k')  => app.sources_move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.sources_move_down(),
        KeyCode::PageUp   => app.sources_page_up(PAGE_SIZE),
        KeyCode::PageDown => app.sources_page_down(PAGE_SIZE),
        KeyCode::Char('a') | KeyCode::Char('A') => {
            app.input_buffer.clear();
            app.screen = Screen::AddSource;
        }
        KeyCode::Delete | KeyCode::Backspace => {
            app.remove_selected_source();
            persist_sources(app);
        }
        _ => {}
    }
}

fn handle_playlists_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Library,
        KeyCode::Up | KeyCode::Char('k')  => app.playlists_move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.playlists_move_down(),
        KeyCode::PageUp   => app.playlists_page_up(PAGE_SIZE),
        KeyCode::PageDown => app.playlists_page_down(PAGE_SIZE),
        KeyCode::Enter                    => app.load_selected_playlist(),
        KeyCode::Delete                   => app.delete_selected_playlist(),
        _ => {}
    }
}

fn handle_conflict_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('o') | KeyCode::Char('O') => app.conflict_overwrite(),
        KeyCode::Char('n') | KeyCode::Char('N') => app.conflict_new_dated(),
        KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Esc => {
            app.conflict_ctx = None;
            app.screen = Screen::Library;
        }
        _ => {}
    }
}

fn handle_transfer_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('q') | KeyCode::Char('Q') => app.running = false,
        KeyCode::Esc | KeyCode::Char('l') | KeyCode::Char('L') => {
            app.screen = Screen::Library;
        }
        _ => {}
    }
}

fn handle_tag_edit_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Enter => app.confirm_tag_edit(),
        KeyCode::Esc   => app.cancel_tag_edit(),
        KeyCode::Backspace => {
            if let Some(state) = &mut app.tag_edit_state {
                state.input.pop();
            }
        }
        KeyCode::Char(c) => {
            if let Some(state) = &mut app.tag_edit_state {
                state.input.push(c);
            }
        }
        _ => {}
    }
}

fn handle_amazon_key(app: &mut App, key: KeyCode, cfg: &mut Config) {
    use crate::app::{AmazonFocus, AmazonOverlay};

    // If an overlay is active, handle text input for it.
    let overlay = app.amazon_state.as_ref().and_then(|s| s.overlay.clone());
    if let Some(ov) = overlay {
        match key {
            KeyCode::Esc => {
                // Cancel → back to library (abandon amazon screen entirely).
                app.amazon_state = None;
                app.input_buffer.clear();
                app.screen = Screen::Library;
            }
            KeyCode::Enter => match ov {
                AmazonOverlay::CookieInput => app.confirm_amazon_cookie(cfg),
                AmazonOverlay::DirInput    => app.confirm_amazon_dir(cfg),
            },
            KeyCode::Backspace => { app.input_buffer.pop(); }
            KeyCode::Char(c)   => app.input_buffer.push(c),
            _ => {}
        }
        return;
    }

    match key {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.amazon_state = None;
            app.screen = Screen::Library;
        }

        KeyCode::Tab => {
            if let Some(state) = &mut app.amazon_state {
                state.focus = match state.focus {
                    AmazonFocus::Catalog => AmazonFocus::Local,
                    AmazonFocus::Local   => AmazonFocus::Catalog,
                };
            }
        }

        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(state) = &mut app.amazon_state {
                match state.focus {
                    AmazonFocus::Catalog => state.move_catalog_up(),
                    AmazonFocus::Local   => state.move_local_up(1),
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(state) = &mut app.amazon_state {
                match state.focus {
                    AmazonFocus::Catalog => state.move_catalog_down(),
                    AmazonFocus::Local   => state.move_local_down(1),
                }
            }
        }
        KeyCode::PageUp => {
            if let Some(state) = &mut app.amazon_state {
                match state.focus {
                    AmazonFocus::Catalog => {
                        for _ in 0..PAGE_SIZE { state.move_catalog_up(); }
                    }
                    AmazonFocus::Local => state.move_local_up(PAGE_SIZE),
                }
            }
        }
        KeyCode::PageDown => {
            if let Some(state) = &mut app.amazon_state {
                match state.focus {
                    AmazonFocus::Catalog => {
                        for _ in 0..PAGE_SIZE { state.move_catalog_down(); }
                    }
                    AmazonFocus::Local => state.move_local_down(PAGE_SIZE),
                }
            }
        }

        // [D] — download the focused Amazon track.
        KeyCode::Char('d') | KeyCode::Char('D') => {
            let (cookie, download_dir) = (app.amazon_cookie.clone(), app.amazon_download_dir.clone());
            if let (Some(cookie), Some(dir), Some(state)) =
                (cookie, download_dir, &mut app.amazon_state)
            {
                if let Some(track) = state.tracks.get(state.catalog_index).cloned() {
                    if !state.downloading.contains(&track.asin)
                        && !state.completed.contains(&track.asin)
                    {
                        state.downloading.insert(track.asin.clone());
                        let inbox = app.amazon_inbox.clone();
                        let client = amazon::AmazonClient::new(cookie);
                        tokio::spawn(async move {
                            client.download_track(track, dir, inbox).await;
                        });
                    }
                }
            }
        }

        // [R] — refresh / re-fetch catalog.
        KeyCode::Char('r') | KeyCode::Char('R') => {
            if let Some(state) = &mut app.amazon_state {
                state.loading = true;
                state.needs_fetch = true;
                state.status = "Fetching catalog…".into();
            }
        }

        _ => {}
    }
}

/// Read the system clipboard and return its text content, if available.
/// Returns None on Android/Termux where no clipboard service exists.
fn clipboard_paste() -> Option<String> {
    #[cfg(not(target_os = "android"))]
    { arboard::Clipboard::new().ok()?.get_text().ok() }
    #[cfg(target_os = "android")]
    { None }
}

/// Generic text-input handler. `on_enter` is called when the user presses Enter.
fn handle_text_input_key(app: &mut App, key: KeyCode, on_enter: impl Fn(&mut App)) {
    match key {
        KeyCode::Esc => {
            app.input_buffer.clear();
            app.screen = Screen::Library;
        }
        KeyCode::Enter     => on_enter(app),
        KeyCode::Backspace => { app.input_buffer.pop(); }
        KeyCode::Char(c)   => app.input_buffer.push(c),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn persist_sources(app: &App) {
    Config {
        source_dirs: app.source_dirs.clone(),
        amazon_cookie: app.amazon_cookie.clone(),
        amazon_download_dir: app.amazon_download_dir.clone(),
    }
    .save();
}

fn refresh_devices(app: &mut App) {
    match device::enumerate_devices() {
        Ok(devs) => {
            let n = devs.len();
            app.devices = devs.into_iter().map(Arc::from).collect();
            app.selected_device = 0;
            app.status_message = Some(if n == 0 {
                "No devices found. Connect a device and press [D].".into()
            } else {
                format!("{n} device(s) found.")
            });
        }
        Err(e) => app.status_message = Some(format!("Device scan error: {e}")),
    }
}
