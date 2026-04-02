//! TUI rendering layer (ratatui).
//!
//! Each screen has its own submodule. This file is the entry point dispatcher
//! plus shared helpers (palette, centered scroll, text truncation).

mod amazon;
mod dedup;
mod library;
mod organize;
mod overlays;
mod playlists;
mod repair;
mod sources;
mod transfer;

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{ListState, Paragraph},
    Frame,
};

use crate::app::{App, Screen};
use crate::library::Track;

// ---------------------------------------------------------------------------
// Palette — accessible to all submodules via `super::`
// ---------------------------------------------------------------------------

const CLR_ACCENT:   Color = Color::Cyan;
const CLR_SELECTED: Color = Color::Yellow;
const CLR_ERROR:    Color = Color::Red;
const CLR_DIM:      Color = Color::DarkGray;
const CLR_SUCCESS:  Color = Color::Green;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn render(app: &App, frame: &mut Frame) {
    let area = frame.area();

    let has_error = app.decoder_error_track.is_some();
    let constraints = if has_error {
        vec![
            Constraint::Length(1), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // error bar
            Constraint::Length(1), // footer
        ]
    } else {
        vec![
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ]
    };

    let areas = Layout::vertical(constraints).split(area);
    let header_area = areas[0];
    let body_area   = areas[1];
    let error_area  = if has_error { Some(areas[2]) } else { None };
    let footer_area = areas[if has_error { 3 } else { 2 }];

    render_header(app, frame, header_area);
    if let (Some(ea), Some(track)) = (error_area, &app.decoder_error_track) {
        render_error_bar(track, frame, ea);
    }
    render_footer(app, frame, footer_area);

    match app.screen {
        Screen::Library => library::render_main(app, frame, body_area),

        Screen::Sources | Screen::AddSource => {
            sources::render_sources(app, frame, body_area);
            if app.screen == Screen::AddSource {
                #[cfg(target_os = "android")]
                let overlay_title = "Add Source Directory  (e.g. /storage/emulated/0/Music)";
                #[cfg(not(target_os = "android"))]
                let overlay_title = "Add Source Directory";
                overlays::render_input_overlay(overlay_title, &app.input_buffer, frame, body_area);
            }
        }

        Screen::Playlists => playlists::render_playlists(app, frame, body_area),

        Screen::SavePlaylist => {
            library::render_main(app, frame, body_area);
            overlays::render_input_overlay("Save Playlist", &app.input_buffer, frame, body_area);
        }

        Screen::PlaylistConflict => playlists::render_playlist_conflict(app, frame, body_area),

        Screen::Transfer => transfer::render_transfer(app, frame, body_area),

        Screen::RepairIpod => repair::render_repair(app, frame, body_area),

        Screen::DeviceTracks => repair::render_device_tracks(app, frame, body_area),

        Screen::EditTrack => {
            library::render_main(app, frame, body_area);
            if let Some(state) = &app.edit_state {
                overlays::render_edit_overlay(state, frame, body_area);
            }
        }

        Screen::Dedup => dedup::render_dedup(app, frame, body_area),

        Screen::Amazon => amazon::render_amazon(app, frame, body_area),

        Screen::Organize => organize::render_organize(app, frame, body_area),
    }

    // Tag edit overlay — floats above whatever screen is active.
    if app.tag_edit_state.is_some() {
        overlays::render_tag_edit_overlay(app, frame, body_area);
    }

    // Search overlay — topmost layer.
    if let Some(state) = &app.search_state {
        overlays::render_search_overlay(state, frame, body_area);
    }

    // Gematria overlay — above everything.
    if let Some(state) = &app.gematria_state {
        overlays::render_gematria_overlay(app, state, frame, body_area);
    }
}

// ---------------------------------------------------------------------------
// Header
// ---------------------------------------------------------------------------

fn render_header(app: &App, frame: &mut Frame, area: Rect) {
    let playlist_tag = app
        .library
        .active_playlist
        .as_deref()
        .map(|n| format!("  ▶ {n}"))
        .unwrap_or_default();

    let title = format!("  console-music-player{playlist_tag}  ");

    let paragraph = Paragraph::new(title)
        .style(Style::default().bg(CLR_ACCENT).fg(Color::Black).bold())
        .alignment(Alignment::Left);
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------------

fn render_error_bar(track: &Track, frame: &mut Frame, area: Rect) {
    use crate::media::MediaItem;
    let label = truncate(
        &format!("  {} — {}", track.display_artist(), track.display_title()),
        area.width.saturating_sub(48) as usize,
    );
    let line = Line::from(vec![
        Span::styled("  ⚠ Decoder error: ", Style::default().fg(Color::White).bold()),
        Span::styled(label, Style::default().fg(Color::White)),
        Span::styled("   [Del] Remove & delete file", Style::default().fg(CLR_ERROR).bold()),
        Span::styled("   [Esc] Dismiss", Style::default().fg(CLR_DIM).bold()),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(CLR_ERROR)),
        area,
    );
}

fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let library_hint;
    let help = if app.gematria_state.is_some() {
        " Type a phrase  [Tab] Cycle system  [Enter] Play selected track  [Esc] Cancel"
    } else if app.search_state.is_some() {
        " [↑↓/jk] Navigate  [PgUp/Dn] Page  [Enter] Jump to track  [Esc] Close  — type to filter"
    } else {
        match app.screen {
            Screen::Library => {
                let r_hint = if app.library.active_playlist.is_some() {
                    "[R] Library"
                } else {
                    "[R] Rescan"
                };
                library_hint = format!(
                    " [↑↓/jk] Nav  [PgUp/Dn] Page  [Enter] Play  [P] Pause  [O] Repeat  \
                     [Z] Sort  []/[ Vol  [Space] Sel  [G] Tag  [/] Search  [\\] Gematria  [M] Organize  [Tab] Pane  {r_hint}"
                );
                library_hint.as_str()
            }
            Screen::Sources    => " [↑↓] Navigate  [A] Add  [Del] Remove  [Esc] Back",
            Screen::AddSource  => " [Enter] Confirm  [Esc] Cancel",
            Screen::Playlists  => " [↑↓] Navigate  [Enter] Load  [Del] Delete  [Esc] Back",
            Screen::SavePlaylist    => " Type a name, then [Enter] to save  [Esc] Cancel",
            Screen::PlaylistConflict => " [O] Overwrite  [N] New dated  [C] Cancel",
            Screen::Transfer   => " [Esc/L] Back to library  [Q] Quit",
            Screen::RepairIpod => " [F] Fix all  [↑↓] Navigate  [Esc] Back",
            Screen::DeviceTracks => " [↑↓] Navigate  [Esc/Q] Back",
            Screen::EditTrack  => " [Tab/↑↓] Next field  [Enter] Save  [Esc] Cancel",
            Screen::Dedup      => " [Tab] Panel  [↑↓] Navigate  [Space] Cycle action  [A] Auto  [Enter] Apply  [Esc] Cancel",
            Screen::Amazon     => " [Tab] Pane  [↑↓] Navigate  [D] Download  [R] Refresh  [?] Diagnostic log  [Esc] Back",
            Screen::Organize   => " [↑↓/jk] Navigate groups  [Enter] Select destination  [Esc] Back",
        }
    };

    let status = app.status_message.as_deref().unwrap_or(help);
    let paragraph = Paragraph::new(status)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Shared helpers — private here, accessible to submodules via `super::`
// ---------------------------------------------------------------------------

/// Build a `ListState` that keeps `selected` vertically centered in `visible_height` rows.
///
/// Near the top/bottom edges the cursor hugs the edge rather than centering —
/// the same behaviour as vim's `scrolloff` set to half the screen height.
fn centered_list_state(selected: usize, total: usize, visible_height: u16) -> ListState {
    let mut state = ListState::default();
    state.select(Some(selected));
    let visible = visible_height as usize;
    if total > visible {
        let half = visible / 2;
        let ideal = selected.saturating_sub(half);
        let max_offset = total.saturating_sub(visible);
        *state.offset_mut() = ideal.min(max_offset);
    }
    state
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
