//! TUI rendering layer (ratatui).

use humansize::{format_size, DECIMAL};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap,
    },
    Frame,
};

use std::path::PathBuf;

use crate::app::{App, DedupFocus, EditState, Focus, Screen, EDIT_FIELD_LABELS};
use crate::library::dedup::{DedupAction, DuplicateKind};
use crate::media::MediaItem;
use crate::player::PlaybackState;
use crate::visualizer;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

const CLR_ACCENT: Color = Color::Cyan;
const CLR_SELECTED: Color = Color::Yellow;
const CLR_ERROR: Color = Color::Red;
const CLR_DIM: Color = Color::DarkGray;
const CLR_SUCCESS: Color = Color::Green;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn render(app: &App, frame: &mut Frame) {
    let area = frame.area();

    let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(app, frame, header_area);
    render_footer(app, frame, footer_area);

    match app.screen {
        Screen::Library => render_main(app, frame, body_area),
        Screen::Sources | Screen::AddSource => render_sources(app, frame, body_area),
        Screen::Playlists => render_playlists(app, frame, body_area),
        Screen::SavePlaylist => {
            render_main(app, frame, body_area);
            render_input_overlay("Save Playlist", &app.input_buffer, frame, body_area);
        }
        Screen::PlaylistConflict => render_playlist_conflict(app, frame, body_area),
        Screen::Transfer => render_transfer(app, frame, body_area),
        Screen::RepairIpod => render_repair(app, frame, body_area),
        Screen::DeviceTracks => render_device_tracks(app, frame, body_area),
        Screen::EditTrack => {
            // Render library in the background, overlay the editor on top.
            render_main(app, frame, body_area);
            if let Some(state) = &app.edit_state {
                render_edit_overlay(state, frame, body_area);
            }
        }
        Screen::Dedup => render_dedup(app, frame, body_area),
    }

    // Tag edit overlay (renders on top of whatever screen is active)
    if app.tag_edit_state.is_some() {
        render_tag_edit_overlay(app, frame, body_area);
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

fn render_footer(app: &App, frame: &mut Frame, area: Rect) {
    let library_hint;
    let help = match app.screen {
        Screen::Library => {
            let r_hint = if app.library.active_playlist.is_some() {
                "[R] Library"
            } else {
                "[R] Rescan"
            };
            library_hint = format!(
                " [↑↓/jk] Nav  [PgUp/Dn] Page  [Enter] Play  [P] Pause  [O] Repeat  \
                 [Z] Sort  []/[ Vol  [Space] Sel  [Tab] Pane  {r_hint}"
            );
            library_hint.as_str()
        }
        Screen::Sources => " [↑↓] Navigate  [A] Add  [Del] Remove  [Esc] Back",
        Screen::AddSource => " [Enter] Confirm  [Esc] Cancel",
        Screen::Playlists => " [↑↓] Navigate  [Enter] Load  [Del] Delete  [Esc] Back",
        Screen::SavePlaylist => " Type a name, then [Enter] to save  [Esc] Cancel",
        Screen::PlaylistConflict => " [O] Overwrite  [N] New dated  [C] Cancel",
        Screen::Transfer => " [Esc/L] Back to library  [Q] Quit",
        Screen::RepairIpod => " [F] Fix all  [↑↓] Navigate  [Esc] Back",
        Screen::DeviceTracks => " [↑↓] Navigate  [Esc/Q] Back",
        Screen::EditTrack => " [Tab/↑↓] Next field  [Enter] Save  [Esc] Cancel",
        Screen::Dedup => " [Tab] Panel  [↑↓] Navigate  [Space] Cycle action  [A] Auto  [Enter] Apply  [Esc] Cancel",
    };

    let status = app.status_message.as_deref().unwrap_or(help);
    let paragraph = Paragraph::new(status)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Main library screen
// ---------------------------------------------------------------------------

fn render_main(app: &App, frame: &mut Frame, area: Rect) {
    let [left, right] = Layout::horizontal([
        Constraint::Percentage(58),
        Constraint::Percentage(42),
    ])
    .areas(area);

    let [player_area, devices_area, functions_area] = Layout::vertical([
        Constraint::Percentage(50),
        Constraint::Percentage(18),
        Constraint::Percentage(32),
    ])
    .areas(right);

    if app.waveform_active {
        render_waveform_pane(app, frame, left);
    } else {
        render_library_pane(app, frame, left);
    }
    render_player_pane(app, frame, player_area);
    render_devices_pane(app, frame, devices_area);
    render_functions_pane(app, frame, functions_area);
}

// ---------------------------------------------------------------------------
// Waveform pane
// ---------------------------------------------------------------------------

fn render_waveform_pane(app: &App, frame: &mut Frame, area: Rect) {
    let title = if let Some(track) = &app.player.current_track {
        format!(" ◈ {} — {} ", track.display_title(), track.display_artist())
    } else {
        " ◈ Waveform — no track playing ".into()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ACCENT))
        .title_style(Style::default().fg(CLR_ACCENT).bold());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width  = inner.width  as usize;
    let height = inner.height as usize;
    if width == 0 || height == 0 {
        return;
    }

    let rows = visualizer::render_waveform(&app.player.wave_buffer, width, height);

    let lines: Vec<Line> = rows
        .into_iter()
        .map(|row| Line::from(Span::styled(row, Style::default().fg(CLR_ACCENT))))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Library pane
// ---------------------------------------------------------------------------

fn render_library_pane(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Library;
    let border_style = if focused {
        Style::default().fg(CLR_ACCENT)
    } else {
        Style::default().fg(CLR_DIM)
    };

    let track_count = app.library.tracks.len();
    let sel_count = app.selected_tracks.len();
    let pl_label = app
        .library
        .active_playlist
        .as_deref()
        .map(|n| format!(" [{n}]"))
        .unwrap_or_default();

    let sort_label = app.library.sort_order.label();
    let title = format!(
        " Library{pl_label} — {track_count} tracks  ↕ {sort_label}{}",
        if sel_count > 0 { format!(" ({sel_count} selected)") } else { String::new() }
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    if app.library.is_empty() {
        let msg = if app.library.active_playlist.is_some() {
            "Playlist is empty or no tracks matched.\n[R] Main Media Library  [L] Playlists"
        } else {
            "No tracks found.\n[S] Manage sources  [L] Load playlist  [R] Rescan"
        };
        let p = Paragraph::new(msg)
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().fg(CLR_DIM));
        frame.render_widget(p, area);
        return;
    }

    // Usable chars per row: pane width minus borders (2) and highlight
    // symbol "> " (2) and selection marker "◆ " (2) = width − 6.
    let avail = area.width.saturating_sub(6) as usize;

    // Ticks before scrolling starts (1.5 s at 100 ms/tick) and speed
    // (1 char every 4 ticks = 400 ms).
    const MARQUEE_DELAY: u32 = 15;
    const MARQUEE_SPEED: u32 = 4;

    // Build the display list.  For GroupBy sort orders, section header rows
    // are injected between groups.  Headers are not selectable, so we track a
    // separate visual_selected index that accounts for the offsets.
    let use_sections = app.library.sort_order.has_sections();

    let mut items: Vec<ListItem> = Vec::with_capacity(app.library.tracks.len() + 8);
    let mut visual_selected: usize = app.library.selected_index;
    let mut last_key: Option<String> = None;

    for (i, track) in app.library.tracks.iter().enumerate() {
        // Inject a section header whenever the group key changes.
        if use_sections {
            if let Some(key) = app.library.section_key(track) {
                if last_key.as_deref() != Some(&key) {
                    let dashes = "─".repeat(avail.saturating_sub(key.len() + 4));
                    let header_text = format!("── {key} {dashes}");
                    items.push(ListItem::new(Line::from(Span::styled(
                        header_text,
                        Style::default().fg(CLR_ACCENT).add_modifier(Modifier::BOLD),
                    ))));
                    last_key = Some(key);
                    if i <= app.library.selected_index {
                        visual_selected += 1;
                    }
                }
            }
        }

        let is_focused = i == app.library.selected_index;
        let selected   = app.is_track_selected(i);
        let marker     = if selected { "◆ " } else { "  " };

        // Compute badge spans and their total character width.
        let (badge_spans, badge_width) = build_badges(app, &track.path);
        let main_avail = avail.saturating_sub(badge_width);

        let full = track.info_line();

        let display = if is_focused && full.chars().count() > main_avail {
            let scroll = if app.marquee_tick > MARQUEE_DELAY {
                ((app.marquee_tick - MARQUEE_DELAY) / MARQUEE_SPEED) as usize
            } else {
                0
            };
            let max_scroll = full.chars().count().saturating_sub(main_avail);
            let offset = scroll.min(max_scroll);
            full.chars().skip(offset).take(main_avail).collect::<String>()
        } else {
            truncate(&full, main_avail)
        };

        let (title_color, meta_color) = if is_focused {
            (Color::White, Color::Gray)
        } else {
            (Color::White, CLR_DIM)
        };

        if let Some(sep) = display.find("  ·  ") {
            let (title_part, rest) = display.split_at(sep);
            let mut spans = vec![
                Span::styled(
                    marker,
                    Style::default().fg(if selected { CLR_SELECTED } else { Color::Reset }),
                ),
                Span::styled(title_part.to_string(), Style::default().fg(title_color).bold()),
                Span::styled(rest.to_string(), Style::default().fg(meta_color)),
            ];
            spans.extend(badge_spans);
            items.push(ListItem::new(Line::from(spans)));
        } else {
            let mut spans = vec![
                Span::styled(
                    marker,
                    Style::default().fg(if selected { CLR_SELECTED } else { Color::Reset }),
                ),
                Span::styled(display, Style::default().fg(title_color)),
            ];
            spans.extend(badge_spans);
            items.push(ListItem::new(Line::from(spans)));
        };
    }

    let mut state = ListState::default();
    state.select(Some(visual_selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Player pane (top-right)
// Split: playing info + progress (top) | focused track metadata (bottom)
// ---------------------------------------------------------------------------

fn render_player_pane(app: &App, frame: &mut Frame, area: Rect) {
    let p = &app.player;
    let icon = p.state.icon();

    let block = Block::default()
        .title(format!(" {icon} Player "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split the inner area: playing section (top) | track info (bottom)
    let [playing_area, divider_area, info_area] = Layout::vertical([
        Constraint::Length(5),   // now-playing: title/artist/album + gauge + time
        Constraint::Length(1),   // divider
        Constraint::Min(0),      // focused track metadata
    ])
    .areas(inner);

    // --- Playing section ---
    if p.state != PlaybackState::Stopped {
        if let Some(track) = &p.current_track {
            let [names_area, gauge_area, time_vol_area] = Layout::vertical([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .areas(playing_area);

            let info_lines = vec![
                Line::styled(
                    truncate(track.display_title(), inner.width as usize),
                    Style::default().fg(Color::White).bold(),
                ),
                Line::styled(
                    truncate(track.display_artist(), inner.width as usize),
                    Style::default().fg(CLR_DIM),
                ),
                Line::styled(
                    truncate(&track.album, inner.width as usize),
                    Style::default().fg(CLR_DIM),
                ),
            ];
            frame.render_widget(Paragraph::new(info_lines), names_area);

            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(CLR_ACCENT).bg(Color::DarkGray))
                .ratio(p.progress());
            frame.render_widget(gauge, gauge_area);

            let elapsed = p.elapsed();
            let elapsed_s = format!("{:02}:{:02}", elapsed.as_secs() / 60, elapsed.as_secs() % 60);
            let total_s = track
                .duration_secs
                .map(|s| format!("{:02}:{:02}", s / 60, s % 60))
                .unwrap_or_else(|| "--:--".into());
            let vol_pct = (p.volume * 100.0).round() as u8;

            let time_vol = Paragraph::new(format!(
                "{elapsed_s}/{total_s}  Vol:{} {vol_pct}%  {}",
                p.volume_bar(),
                p.repeat.icon()
            ))
            .style(Style::default().fg(CLR_DIM))
            .alignment(Alignment::Center);
            frame.render_widget(time_vol, time_vol_area);
        }
    } else {
        let idle = Paragraph::new("─ stopped ─")
            .style(Style::default().fg(CLR_DIM))
            .alignment(Alignment::Center);
        frame.render_widget(idle, playing_area);
    }

    // --- Divider ---
    let div = Paragraph::new("─".repeat(inner.width as usize))
        .style(Style::default().fg(CLR_DIM));
    frame.render_widget(div, divider_area);

    // --- Focused track metadata ---
    render_track_metadata(app, frame, info_area);
}

fn render_track_metadata(app: &App, frame: &mut Frame, area: Rect) {
    let Some(track) = app.library.tracks.get(app.library.selected_index) else {
        let empty = Paragraph::new("No track focused.")
            .style(Style::default().fg(CLR_DIM))
            .alignment(Alignment::Center);
        frame.render_widget(empty, area);
        return;
    };

    let w = area.width as usize;

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Title  ", Style::default().fg(CLR_DIM)),
            Span::styled(track.display_title(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Artist ", Style::default().fg(CLR_DIM)),
            Span::styled(track.display_artist(), Style::default().fg(Color::White)),
        ]),
    ];

    if !track.album.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Album  ", Style::default().fg(CLR_DIM)),
            Span::styled(&track.album, Style::default().fg(Color::White)),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Time   ", Style::default().fg(CLR_DIM)),
        Span::styled(track.display_duration(), Style::default().fg(Color::White)),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Format ", Style::default().fg(CLR_DIM)),
        Span::styled(
            {
                let fmt = track.format_label().to_uppercase();
                let br = track
                    .bitrate_kbps
                    .map(|b| format!(" {b} kbps"))
                    .unwrap_or_default();
                let sr = track
                    .sample_rate_hz
                    .map(|r| format!("  {r} Hz"))
                    .unwrap_or_default();
                let ch: String = track
                    .channels
                    .map(|c| if c == 1 { "  mono".to_string() } else { "  stereo".to_string() })
                    .unwrap_or_default();
                format!("{fmt}{br}{sr}{ch}")
            },
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Size   ", Style::default().fg(CLR_DIM)),
        Span::styled(
            format_size(track.file_size, DECIMAL),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Path   ", Style::default().fg(CLR_DIM)),
        Span::styled(
            truncate(&track.path.display().to_string(), w.saturating_sub(8)),
            Style::default().fg(CLR_DIM),
        ),
    ]));

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        area,
    );
}

// ---------------------------------------------------------------------------
// Devices pane (bottom-right)
// ---------------------------------------------------------------------------

fn render_devices_pane(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Devices;
    let border_style = if focused {
        Style::default().fg(CLR_ACCENT)
    } else {
        Style::default().fg(CLR_DIM)
    };

    let block = Block::default()
        .title(" Devices ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);

    if app.devices.is_empty() {
        let msg = Paragraph::new("No devices.\nConnect iPod/iPhone\npress [D] to refresh.")
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().fg(CLR_DIM));
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .devices
        .iter()
        .map(|dev| {
            let fw = dev.firmware_label();
            let fw_span = if fw.is_empty() {
                String::new()
            } else {
                format!("  {fw}")
            };
            let space = dev
                .free_space()
                .map(|b| format!(" — {} free", format_size(b, DECIMAL)))
                .unwrap_or_default();
            let line = Line::from(vec![
                Span::styled(format!("{} ", dev.kind()), Style::default().fg(CLR_ACCENT)),
                Span::styled(dev.name().to_string(), Style::default().fg(Color::White)),
                Span::styled(fw_span, Style::default().fg(CLR_SUCCESS)),
                Span::styled(space, Style::default().fg(CLR_DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.selected_device));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Functions pane (bottom-right)
// ---------------------------------------------------------------------------

fn render_functions_pane(_app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Functions ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_DIM));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Two sections: iPod actions | Library actions
    let key = |k: &str| Span::styled(format!("[{k}]"), Style::default().fg(CLR_ACCENT).bold());
    let label = |s: &str| Span::styled(format!(" {s}", ), Style::default().fg(Color::White));
    let dim = |s: &str| Span::styled(s.to_string(), Style::default().fg(CLR_DIM));

    let lines = vec![
        Line::from(vec![dim("── iPod ──────────────────")]),
        Line::from(vec![key("T"), label("Transfer selected tracks")]),
        Line::from(vec![key("N"), label("Init new iTunesDB")]),
        Line::from(vec![key("X"), label("Repair iPod")]),
        Line::from(vec![key("I"), label("Browse iPod library")]),
        Line::from(vec![key("U"), label("Dump iTunesDB to log")]),
        Line::from(vec![dim("── Library ───────────────")]),
        Line::from(vec![key("S"), label("Sources  "), key("L"), label("Playlists")]),
        Line::from(vec![key("W"), label("Save PL  "), key("D"), label("Rescan devs")]),
        Line::from(vec![key("E"), label("Edit tags"), key("R"), label("Rescan")]),
        Line::from(vec![key("F"), label("Find dupes"), key("G"), label("Tag")]),
        Line::from(vec![key("Z"), label("Sort      "), key("O"), label("Repeat")]),
        Line::from(vec![key("V"), label("Waveform")]),
        Line::from(vec![key("Q"), label("Quit")]),
    ];

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

// ---------------------------------------------------------------------------
// Sources screen
// ---------------------------------------------------------------------------

fn render_sources(app: &App, frame: &mut Frame, area: Rect) {
    let title = format!(
        " Sources — {} director{}",
        app.source_dirs.len(),
        if app.source_dirs.len() == 1 { "y" } else { "ies" }
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ACCENT));

    if app.source_dirs.is_empty() {
        let msg = Paragraph::new("No source directories.\nPress [A] to add one.")
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().fg(CLR_DIM));
        frame.render_widget(msg, area);
    } else {
        let items: Vec<ListItem> = app
            .source_dirs
            .iter()
            .map(|p| ListItem::new(p.display().to_string()))
            .collect();

        let mut state = ListState::default();
        state.select(Some(app.sources_selected));

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().bg(Color::DarkGray).bold())
            .highlight_symbol("> ");

        frame.render_stateful_widget(list, area, &mut state);
    }

    if app.screen == Screen::AddSource {
        render_input_overlay("Add Source Directory", &app.input_buffer, frame, area);
    }
}

// ---------------------------------------------------------------------------
// Playlists screen
// ---------------------------------------------------------------------------

fn render_playlists(app: &App, frame: &mut Frame, area: Rect) {
    let title = format!(" Playlists — {} saved ", app.playlist_names.len());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ACCENT));

    if app.playlist_names.is_empty() {
        let msg = Paragraph::new(
            "No playlists saved.\nSelect tracks in the library,\nthen press [W] to save one.",
        )
        .block(block)
        .alignment(Alignment::Center)
        .style(Style::default().fg(CLR_DIM));
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .playlist_names
        .iter()
        .map(|name| ListItem::new(name.as_str()))
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.playlists_selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Playlist conflict screen
// ---------------------------------------------------------------------------

fn render_playlist_conflict(app: &App, frame: &mut Frame, area: Rect) {
    let Some(ctx) = &app.conflict_ctx else { return };

    let block = Block::default()
        .title(" Playlist Name Conflict ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ERROR));

    let existing_count = ctx.existing_tracks.len();
    let new_count = ctx.new_tracks.len();
    let merged_count = {
        let mut merged = ctx.existing_tracks.clone();
        for t in &ctx.new_tracks {
            if !merged.contains(t) {
                merged.push(t.clone());
            }
        }
        merged.len()
    };

    let text = vec![
        Line::raw(""),
        Line::styled(
            format!("  '{}' already exists.", ctx.name),
            Style::default().fg(Color::White).bold(),
        ),
        Line::raw(""),
        Line::styled(
            format!("  Existing: {existing_count} tracks"),
            Style::default().fg(CLR_DIM),
        ),
        Line::styled(
            format!("  New:      {new_count} tracks"),
            Style::default().fg(CLR_DIM),
        ),
        Line::raw(""),
        Line::styled(
            "  [O]  Overwrite — replace with new tracks",
            Style::default().fg(CLR_ACCENT),
        ),
        Line::styled(
            format!("  [N]  New dated  — merged ({merged_count} unique tracks)"),
            Style::default().fg(CLR_ACCENT),
        ),
        Line::styled(
            "  [C]  Cancel",
            Style::default().fg(CLR_DIM),
        ),
    ];

    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}

// ---------------------------------------------------------------------------
// Transfer log
// ---------------------------------------------------------------------------

fn render_transfer(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Transfer Log ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ACCENT));

    let lines: Vec<Line> = app
        .transfer_log
        .iter()
        .map(|s| {
            let color = if s.contains('✓') { CLR_SUCCESS } else if s.contains('✗') { CLR_ERROR } else { Color::White };
            Line::styled(s.as_str(), Style::default().fg(color))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        area,
    );
}

// ---------------------------------------------------------------------------
// iPod Repair screen
// ---------------------------------------------------------------------------

fn render_repair(app: &App, frame: &mut Frame, area: Rect) {
    let Some(results) = &app.repair_results else {
        let msg = Paragraph::new("No scan results. Press Esc.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(CLR_DIM));
        frame.render_widget(msg, area);
        return;
    };

    let issue_count = results.issue_count();
    let title = if issue_count == 0 {
        " iPod Health — All Good ".to_string()
    } else {
        format!(" iPod Repair — {issue_count} issue(s) found ")
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if issue_count == 0 { CLR_SUCCESS } else { CLR_ERROR }));

    if issue_count == 0 {
        let msg = Paragraph::new(
            "\n  No orphaned files and no missing playlist entries.\n  Your iPod database is consistent.",
        )
        .block(block)
        .style(Style::default().fg(CLR_SUCCESS));
        frame.render_widget(msg, area);
        return;
    }

    // Build a flat list of all issues
    let mut items: Vec<ListItem> = Vec::new();

    if !results.incomplete_entries.is_empty() {
        items.push(ListItem::new(Line::styled(
            format!("  ── {} missing playlist entr(ies) ──", results.incomplete_entries.len()),
            Style::default().fg(CLR_DIM),
        )));
        for entry in &results.incomplete_entries {
            let label = if entry.title.is_empty() {
                entry.ipod_rel_path.clone()
            } else {
                format!("{} ({})", entry.title, entry.ipod_rel_path)
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled("  ⚠ ", Style::default().fg(Color::Yellow)),
                Span::styled(truncate(&label, area.width as usize - 6), Style::default().fg(Color::White)),
            ])));
        }
    }

    if !results.orphaned_files.is_empty() {
        items.push(ListItem::new(Line::styled(
            format!("  ── {} orphaned file(s) (not in DB) ──", results.orphaned_files.len()),
            Style::default().fg(CLR_DIM),
        )));
        for orphan in &results.orphaned_files {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("  ○ ", Style::default().fg(CLR_ACCENT)),
                Span::styled(
                    truncate(&orphan.ipod_rel_path, area.width as usize - 6),
                    Style::default().fg(Color::White),
                ),
            ])));
        }
    }

    let mut state = ListState::default();
    state.select(Some(app.repair_selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Device track browser
// ---------------------------------------------------------------------------

fn render_device_tracks(app: &App, frame: &mut Frame, area: Rect) {
    let track_count = app.device_tracks.len();
    let from_db = app.device_tracks.first().map(|t| t.from_db).unwrap_or(false);
    let source_tag = if from_db { " iTunesDB" } else { " filesystem scan" };

    let title = format!(" iPod Library — {track_count} tracks ({source_tag}) ");

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_ACCENT));

    if app.device_tracks.is_empty() {
        let msg = if from_db {
            "No tracks found in iTunesDB.\nTry transferring tracks first."
        } else {
            "No audio files found on device.\nConnect the iPod and press [D] to refresh."
        };
        let p = Paragraph::new(msg)
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().fg(CLR_DIM));
        frame.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = app
        .device_tracks
        .iter()
        .map(|t| {
            let dur = if t.duration_ms > 0 {
                let secs = t.duration_ms / 1000;
                format!(" {:02}:{:02}", secs / 60, secs % 60)
            } else {
                String::new()
            };
            // Derive format badge from the file extension in the iPod path
            let fmt_badge = std::path::Path::new(&t.ipod_rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("[{}]", e.to_uppercase()))
                .unwrap_or_default();

            let (db_marker, marker_color) = if t.from_db {
                ("●", CLR_SUCCESS)
            } else {
                ("○", CLR_DIM)
            };

            // For filesystem entries with no artist/album, show the folder path instead
            let second_col = if !t.artist.is_empty() {
                truncate(&t.artist, 22)
            } else {
                // Show e.g. "F00/" as the folder context
                std::path::Path::new(&t.ipod_rel_path)
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| format!("{s}/"))
                    .unwrap_or_default()
            };

            let line = Line::from(vec![
                Span::styled(
                    format!("{db_marker} "),
                    Style::default().fg(marker_color),
                ),
                Span::styled(
                    format!("{:<6}", fmt_badge),
                    Style::default().fg(CLR_ACCENT),
                ),
                Span::styled(
                    format!(" {:<30}", truncate(&t.title, 28)),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(" {:<22}", second_col),
                    Style::default().fg(CLR_DIM),
                ),
                Span::styled(
                    if !t.album.is_empty() {
                        format!(" {:<16}", truncate(&t.album, 14))
                    } else {
                        String::new()
                    },
                    Style::default().fg(CLR_DIM),
                ),
                Span::styled(dur, Style::default().fg(CLR_DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.device_tracks_selected));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Tag editor overlay
// ---------------------------------------------------------------------------

/// Centered multi-field popup for editing track metadata.
///
/// Layout (inside the popup border):
///   one row per field (label + editable value)
///   one row for the key-hint line
fn render_edit_overlay(state: &EditState, frame: &mut Frame, parent: Rect) {
    // 5 field rows + 1 blank separator + 1 hint = 7 inner rows + 2 border = 9 total
    let height: u16 = 9;
    let width = (parent.width * 3 / 5).max(54).min(parent.width.saturating_sub(4));
    let x = parent.x + (parent.width.saturating_sub(width)) / 2;
    let y = parent.y + (parent.height.saturating_sub(height)) / 2;
    let popup = Rect { x, y, width, height };

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Edit Track Metadata ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CLR_SELECTED));

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let [f0, f1, f2, f3, f4, _blank, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let field_areas = [f0, f1, f2, f3, f4];
    let val_width = inner.width.saturating_sub(8) as usize; // 8 = label width

    for (i, (label, value)) in EDIT_FIELD_LABELS.iter().zip(state.fields.iter()).enumerate() {
        let focused = i == state.focused_field;
        let display = truncate(value, val_width.saturating_sub(1));
        let cursor = if focused { "_" } else { "" };

        let line = Line::from(vec![
            Span::styled(*label, Style::default().fg(CLR_DIM)),
            Span::styled(
                format!("{display}{cursor}"),
                if focused {
                    Style::default().fg(CLR_SELECTED).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]);
        frame.render_widget(Paragraph::new(line), field_areas[i]);
    }

    frame.render_widget(
        Paragraph::new("[Tab/↑↓] Next field  [Enter] Save  [Esc] Cancel")
            .style(Style::default().fg(CLR_DIM)),
        hint_area,
    );
}

// ---------------------------------------------------------------------------
// Shared input overlay
// ---------------------------------------------------------------------------

fn render_input_overlay(title: &str, buffer: &str, frame: &mut Frame, parent: Rect) {
    let width = (parent.width * 3 / 5).max(40).min(parent.width.saturating_sub(4));
    let x = parent.x + (parent.width.saturating_sub(width)) / 2;
    let y = parent.y + parent.height / 2 - 1;
    let popup = Rect { x, y, width, height: 3 };

    frame.render_widget(Clear, popup);

    let content = format!("{buffer}_");
    let paragraph = Paragraph::new(content)
        .block(
            Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(CLR_SELECTED)),
        )
        .style(Style::default().fg(Color::White));

    frame.render_widget(paragraph, popup);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Deduplication screen
// ---------------------------------------------------------------------------
//
// Layout: left panel (35 %) = scrollable group list
//         right panel (65%) = candidate detail for the focused group
//
// Navigation:
//   Tab          — switch focus between panels
//   ↑/↓ / j/k   — scroll groups (left) or candidates (right)
//   Space        — cycle action for focused candidate (right panel)
//   A            — auto-suggest all groups
//   Enter        — apply deletions
//   Esc          — cancel, return to library

fn render_dedup(app: &App, frame: &mut Frame, area: Rect) {
    let Some(state) = &app.dedup_state else { return };

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).areas(area);

    // ── Left: group list ────────────────────────────────────────────────────
    let group_items: Vec<ListItem> = state
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let kind_tag = match g.kind {
                DuplicateKind::ExactContent  => "=",
                DuplicateKind::MetadataMatch => "~",
            };
            let title = g
                .candidates
                .first()
                .map(|c| truncate(&c.track.title, 22))
                .unwrap_or_default();
            let del_count = state.actions.get(i)
                .map(|acts| acts.iter().filter(|&&a| a == DedupAction::Delete).count())
                .unwrap_or(0);
            let del_tag = if del_count > 0 { format!(" -{del_count}") } else { String::new() };

            let text = format!("{kind_tag} {title}{del_tag}");
            let style = if i == state.group_index {
                if state.focus == DedupFocus::Groups {
                    Style::default().fg(CLR_SELECTED).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(CLR_DIM)
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let left_border = if state.focus == DedupFocus::Groups { CLR_SELECTED } else { CLR_DIM };
    let groups_widget = List::new(group_items).block(
        Block::default()
            .title(format!(
                " Duplicates — {} group(s)  [=]exact [~]meta ",
                state.group_count()
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(left_border)),
    );

    let mut list_state = ListState::default();
    list_state.select(Some(state.group_index));
    frame.render_stateful_widget(groups_widget, left, &mut list_state);

    // ── Right: candidate detail ──────────────────────────────────────────────
    if let (Some(group), Some(actions)) = (state.focused_group(), state.focused_actions()) {
        let kind_label = match group.kind {
            DuplicateKind::ExactContent  => "Exact content match (identical bytes)",
            DuplicateKind::MetadataMatch => "Metadata match (same title + artist)",
        };

        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                Span::styled("  Kind: ", Style::default().fg(CLR_DIM)),
                Span::styled(kind_label, Style::default().fg(CLR_ACCENT)),
            ]),
            Line::raw(""),
        ];

        for (ci, (candidate, &action)) in
            group.candidates.iter().zip(actions.iter()).enumerate()
        {
            let focused = ci == state.candidate_index && state.focus == DedupFocus::Candidates;
            let action_style = match action {
                DedupAction::Keep      => Style::default().fg(CLR_SUCCESS).bold(),
                DedupAction::Delete    => Style::default().fg(CLR_ERROR).bold(),
                DedupAction::Undecided => Style::default().fg(CLR_DIM),
            };
            let selector = if focused { "▶ " } else { "  " };

            lines.push(Line::from(vec![
                Span::raw(selector),
                Span::styled(
                    format!("[{}]", action.label()),
                    action_style,
                ),
                Span::raw(format!("  #{}", ci + 1)),
            ]));

            let t = &candidate.track;
            let path_str = truncate(&t.path.to_string_lossy(), right.width as usize - 6);
            lines.push(Line::from(Span::styled(
                format!("     {path_str}"),
                Style::default().fg(Color::White),
            )));

            let mut meta = Vec::new();
            if !t.title.is_empty() { meta.push(t.title.clone()); }
            if !t.artist.is_empty() { meta.push(t.artist.clone()); }
            if !t.album.is_empty() {
                let a = match t.year {
                    Some(y) => format!("{} ({})", t.album, y),
                    None    => t.album.clone(),
                };
                meta.push(a);
            }
            if !meta.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("     {}", meta.join(" · ")),
                    Style::default().fg(CLR_DIM),
                )));
            }

            let mut info = Vec::new();
            info.push(format_size(t.file_size, DECIMAL));
            if let Some(d) = t.duration_secs {
                info.push(format!("{}:{:02}", d / 60, d % 60));
            }
            if let Some(br) = t.bitrate_kbps {
                info.push(format!("{br} kbps"));
            }
            if let Some(cs) = candidate.checksum {
                info.push(format!("fp:{cs:016x}"));
            }
            lines.push(Line::from(Span::styled(
                format!("     {}", info.join("  ")),
                Style::default().fg(CLR_DIM),
            )));
            lines.push(Line::raw(""));
        }

        let to_delete = actions.iter().filter(|&&a| a == DedupAction::Delete).count();
        let summary = format!(
            "  Group {}/{} — {} to delete total across all groups",
            state.group_index + 1,
            state.group_count(),
            state.to_delete_count(),
        );
        lines.push(Line::from(Span::styled(summary, Style::default().fg(CLR_ACCENT))));
        let _ = to_delete;

        let right_border = if state.focus == DedupFocus::Candidates { CLR_SELECTED } else { CLR_DIM };
        let detail = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Candidates — [Space] cycle action  [A] auto-suggest ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(right_border)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(detail, right);
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let t: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

/// Build playlist + tag badge spans for a track, plus their total display width.
///
/// Playlist badges look like: `‹Name›` in blue
/// Tag badges look like:       `#name`  in magenta
fn build_badges<'a>(app: &'a App, path: &PathBuf) -> (Vec<Span<'a>>, usize) {
    let mut spans: Vec<Span<'a>> = Vec::new();
    let mut width: usize = 0;

    // Playlist badges — cap at 2, show "+N" overflow indicator.
    let playlists: &[String] = app.playlist_membership
        .get(path)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let shown_pl = playlists.len().min(2);
    for (i, pl) in playlists.iter().take(shown_pl).enumerate() {
        if i > 0 { spans.push(Span::raw(" ")); width += 1; }
        let badge = format!("‹{pl}›");
        width += badge.chars().count();
        spans.push(Span::styled(badge, Style::default().fg(Color::Blue).bold()));
    }
    if playlists.len() > 2 {
        let more = format!("+{}", playlists.len() - 2);
        spans.push(Span::raw(" "));
        width += 1 + more.len();
        spans.push(Span::styled(more, Style::default().fg(Color::Blue)));
    }

    // Tag badges — cap at 3, show "+N" overflow.
    let tags = app.tag_store.tags_for(path);
    if !tags.is_empty() {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
            width += 2;
        }
        let shown_tags = tags.len().min(3);
        for (i, tag) in tags.iter().take(shown_tags).enumerate() {
            if i > 0 { spans.push(Span::raw(" ")); width += 1; }
            let badge = format!("#{tag}");
            width += badge.chars().count();
            spans.push(Span::styled(badge, Style::default().fg(Color::Magenta)));
        }
        if tags.len() > 3 {
            let more = format!("+{}", tags.len() - 3);
            spans.push(Span::raw(" "));
            width += 1 + more.len();
            spans.push(Span::styled(more, Style::default().fg(Color::Magenta)));
        }
    }

    if !spans.is_empty() {
        spans.insert(0, Span::raw("  "));
        width += 2;
    }

    (spans, width)
}

// ---------------------------------------------------------------------------
// Tag edit overlay
// ---------------------------------------------------------------------------

fn render_tag_edit_overlay(app: &App, frame: &mut Frame, parent: Rect) {
    let Some(state) = &app.tag_edit_state else { return };

    // Center the overlay: 60% wide, 12 rows tall.
    let width  = (parent.width  * 60 / 100).max(50);
    let height = 12u16;
    let x = parent.x + (parent.width.saturating_sub(width))  / 2;
    let y = parent.y + (parent.height.saturating_sub(height)) / 2;
    let area = Rect { x, y, width, height };

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Tag Editor ")
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Magenta))
        .title_style(Style::default().fg(Color::Magenta).bold());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Track display name
    let name_line = Line::from(Span::styled(
        truncate(&state.display_name, inner.width as usize),
        Style::default().fg(Color::White).bold(),
    ));

    // Current tags as badges
    let current_tags = app.tag_store.tags_for(&state.path);
    let tag_spans: Vec<Span> = if current_tags.is_empty() {
        vec![Span::styled("  (none)", Style::default().fg(CLR_DIM))]
    } else {
        let mut s = vec![Span::raw("  ")];
        for (i, tag) in current_tags.iter().enumerate() {
            if i > 0 { s.push(Span::raw("  ")); }
            s.push(Span::styled(
                format!("#{tag}"),
                Style::default().fg(Color::Magenta).bold(),
            ));
        }
        s
    };
    let tags_label = Line::from(vec![
        Span::styled("Tags: ", Style::default().fg(CLR_DIM)),
    ]);
    let tags_line = Line::from(tag_spans);

    // Input field
    let input_label = Line::from(Span::styled(
        "Edit (comma-separated):",
        Style::default().fg(CLR_DIM),
    ));
    let input_w = inner.width.saturating_sub(4) as usize;
    let input_display = if state.input.chars().count() > input_w {
        let skip = state.input.chars().count() - input_w;
        state.input.chars().skip(skip).collect::<String>()
    } else {
        state.input.clone()
    };
    let cursor = format!("{input_display}▌");
    let input_line = Line::from(Span::styled(
        format!(" {} ", cursor),
        Style::default().fg(Color::White).bg(Color::DarkGray),
    ));

    // Controls
    let ctrl = Line::from(vec![
        Span::styled("[Enter]", Style::default().fg(Color::Magenta).bold()),
        Span::raw(" Save  "),
        Span::styled("[Esc]", Style::default().fg(CLR_DIM).bold()),
        Span::raw(" Cancel"),
    ]);

    let content = Text::from(vec![
        name_line,
        Line::default(),
        tags_label,
        tags_line,
        Line::default(),
        input_label,
        input_line,
        Line::default(),
        ctrl,
    ]);

    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: false }), inner);
}
