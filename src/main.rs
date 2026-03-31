//! console-music-player — entry point.
#![allow(dead_code)]

mod app;
mod config;
mod device;
mod error;
mod library;
mod media;
mod player;
mod playlist;
mod tracker;
mod transfer;
mod ui;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
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

    let _audio_stream = rodio::OutputStream::try_default().ok();
    let audio_handle = _audio_stream.as_ref().map(|(_, h)| h.clone());
    if audio_handle.is_none() {
        eprintln!("Warning: no audio output device — playback unavailable.");
    }

    let mut app = App::new(cfg.source_dirs.clone(), audio_handle);
    refresh_devices(&mut app);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut app, &mut terminal).await;

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
) -> anyhow::Result<()> {
    const TICK: Duration = Duration::from_millis(100);

    while app.running {
        terminal.draw(|frame| ui::render(app, frame))?;
        app.tick();

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                // Don't wipe status while the user is typing
                let is_input_screen = matches!(
                    app.screen,
                    Screen::AddSource | Screen::SavePlaylist | Screen::EditTrack
                );
                if !is_input_screen {
                    app.status_message = None;
                }
                handle_key(app, key.code);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Key dispatch
// ---------------------------------------------------------------------------

fn handle_key(app: &mut App, key: KeyCode) {
    match app.screen {
        Screen::Library         => handle_library_key(app, key),
        Screen::EditTrack       => handle_edit_key(app, key),
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
    }
}

fn handle_library_key(app: &mut App, key: KeyCode) {
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
        _ => {}
    }
}

fn handle_sources_key(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Library,
        KeyCode::Up | KeyCode::Char('k')  => app.sources_move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.sources_move_down(),
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
    Config { source_dirs: app.source_dirs.clone() }.save();
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
