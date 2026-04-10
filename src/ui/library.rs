//! Library screen: track list, player pane, devices pane, functions pane, waveform.

use humansize::{format_size, DECIMAL};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame,
};
use std::path::PathBuf;

use crate::app::{App, Focus};
use crate::media::MediaItem;
use crate::p2p::P2pBufferState;
use crate::player::PlaybackState;
use crate::visualizer;

// ---------------------------------------------------------------------------
// Main layout
// ---------------------------------------------------------------------------

pub(super) fn render_main(app: &App, frame: &mut Frame, area: Rect) {
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

pub(super) fn render_waveform_pane(app: &App, frame: &mut Frame, area: Rect) {
    let title = if let Some(track) = &app.player.current_track {
        format!(" ◈ {} — {} ", track.display_title(), track.display_artist())
    } else {
        " ◈ Waveform — no track playing ".into()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT))
        .title_style(Style::default().fg(super::CLR_ACCENT).bold());

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
        .map(|row| Line::from(Span::styled(row, Style::default().fg(super::CLR_ACCENT))))
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Library pane
// ---------------------------------------------------------------------------

pub(super) fn render_library_pane(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Library;
    let border_style = if focused {
        Style::default().fg(super::CLR_ACCENT)
    } else {
        Style::default().fg(super::CLR_DIM)
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
            .style(Style::default().fg(super::CLR_DIM));
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
                        Style::default().fg(super::CLR_ACCENT).add_modifier(Modifier::BOLD),
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
            super::truncate(&full, main_avail)
        };

        let (title_color, meta_color) = if is_focused {
            (Color::White, Color::Gray)
        } else {
            (Color::White, super::CLR_DIM)
        };

        if let Some(sep) = display.find("  ·  ") {
            let (title_part, rest) = display.split_at(sep);
            let mut spans = vec![
                Span::styled(
                    marker,
                    Style::default().fg(if selected { super::CLR_SELECTED } else { Color::Reset }),
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
                    Style::default().fg(if selected { super::CLR_SELECTED } else { Color::Reset }),
                ),
                Span::styled(display, Style::default().fg(title_color)),
            ];
            spans.extend(badge_spans);
            items.push(ListItem::new(Line::from(spans)));
        };
    }

    let mut state = super::centered_list_state(
        visual_selected,
        items.len(),
        area.height.saturating_sub(2),
    );

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

pub(super) fn render_player_pane(app: &App, frame: &mut Frame, area: Rect) {
    let p = &app.player;
    let icon = p.state.icon();

    let block = Block::default()
        .title(format!(" {icon} Player "))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [playing_area, divider_area, info_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    // ── P2P buffering states (override normal player display) ────────────
    match &app.p2p_buffer_state {
        P2pBufferState::Requesting { peer_nick, .. } => {
            render_p2p_requesting(peer_nick, inner.width, frame, playing_area);
        }
        P2pBufferState::Buffering { peer_nick, received, total, stalled, .. } => {
            render_p2p_buffering(peer_nick, *received, *total, *stalled, inner.width, frame, playing_area);
        }
        _ => {
            // Normal local playback OR remote track playing.
            if p.state != PlaybackState::Stopped {
                // Prefer remote track metadata if available.
                let (title, artist, album, duration_secs) =
                    if let Some(rt) = &p.current_remote {
                        (rt.title.as_str(), rt.artist.as_str(), rt.album.as_str(), rt.duration_secs)
                    } else if let Some(lt) = &p.current_track {
                        (lt.display_title(), lt.display_artist(), lt.album.as_str(), lt.duration_secs)
                    } else {
                        ("", "", "", None)
                    };

                let [names_area, gauge_area, time_vol_area] = Layout::vertical([
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .areas(playing_area);

                let info_lines = vec![
                    Line::styled(
                        super::truncate(title, inner.width as usize),
                        Style::default().fg(Color::White).bold(),
                    ),
                    Line::styled(
                        super::truncate(artist, inner.width as usize),
                        Style::default().fg(super::CLR_DIM),
                    ),
                    Line::styled(
                        super::truncate(album, inner.width as usize),
                        Style::default().fg(super::CLR_DIM),
                    ),
                ];
                frame.render_widget(Paragraph::new(info_lines), names_area);

                let gauge = Gauge::default()
                    .gauge_style(Style::default().fg(super::CLR_ACCENT).bg(Color::DarkGray))
                    .ratio(p.progress());
                frame.render_widget(gauge, gauge_area);

                let elapsed = p.elapsed();
                let elapsed_s = format!("{:02}:{:02}", elapsed.as_secs() / 60, elapsed.as_secs() % 60);
                let total_s = duration_secs
                    .map(|s| format!("{:02}:{:02}", s / 60, s % 60))
                    .unwrap_or_else(|| "--:--".into());
                let vol_pct = (p.volume * 100.0).round() as u8;

                // Remote badge: [⬡ @nick] appended to the time/vol line.
                let remote_badge = if let P2pBufferState::Playing { peer_nick } = &app.p2p_buffer_state {
                    format!("  [⬡ @{peer_nick}]")
                } else {
                    String::new()
                };

                let time_vol = Paragraph::new(format!(
                    "{elapsed_s}/{total_s}  Vol:{} {vol_pct}%  {}{}{}",
                    p.volume_bar(),
                    p.repeat.icon(),
                    p.shuffle.icon(),
                    remote_badge,
                ))
                .style(Style::default().fg(super::CLR_DIM))
                .alignment(Alignment::Center);
                frame.render_widget(time_vol, time_vol_area);
            } else {
                let idle = Paragraph::new("─ stopped ─")
                    .style(Style::default().fg(super::CLR_DIM))
                    .alignment(Alignment::Center);
                frame.render_widget(idle, playing_area);
            }
        }
    }

    let div = Paragraph::new("─".repeat(inner.width as usize))
        .style(Style::default().fg(super::CLR_DIM));
    frame.render_widget(div, divider_area);

    render_track_metadata(app, frame, info_area);
}

fn render_track_metadata(app: &App, frame: &mut Frame, area: Rect) {
    let Some(track) = app.library.tracks.get(app.library.selected_index) else {
        let empty = Paragraph::new("No track focused.")
            .style(Style::default().fg(super::CLR_DIM))
            .alignment(Alignment::Center);
        frame.render_widget(empty, area);
        return;
    };

    let w = area.width as usize;

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Title  ", Style::default().fg(super::CLR_DIM)),
            Span::styled(track.display_title(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Artist ", Style::default().fg(super::CLR_DIM)),
            Span::styled(track.display_artist(), Style::default().fg(Color::White)),
        ]),
    ];

    if !track.album.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Album  ", Style::default().fg(super::CLR_DIM)),
            Span::styled(&track.album, Style::default().fg(Color::White)),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Time   ", Style::default().fg(super::CLR_DIM)),
        Span::styled(track.display_duration(), Style::default().fg(Color::White)),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Format ", Style::default().fg(super::CLR_DIM)),
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
        Span::styled("Size   ", Style::default().fg(super::CLR_DIM)),
        Span::styled(
            format_size(track.file_size, DECIMAL),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("Path   ", Style::default().fg(super::CLR_DIM)),
        Span::styled(
            super::truncate(&track.path.display().to_string(), w.saturating_sub(8)),
            Style::default().fg(super::CLR_DIM),
        ),
    ]));

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        area,
    );
}

// ---------------------------------------------------------------------------
// Devices pane
// ---------------------------------------------------------------------------

pub(super) fn render_devices_pane(app: &App, frame: &mut Frame, area: Rect) {
    let focused = app.focus == Focus::Devices;
    let border_style = if focused {
        Style::default().fg(super::CLR_ACCENT)
    } else {
        Style::default().fg(super::CLR_DIM)
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
            .style(Style::default().fg(super::CLR_DIM));
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
                Span::styled(format!("{} ", dev.kind()), Style::default().fg(super::CLR_ACCENT)),
                Span::styled(dev.name().to_string(), Style::default().fg(Color::White)),
                Span::styled(fw_span, Style::default().fg(super::CLR_SUCCESS)),
                Span::styled(space, Style::default().fg(super::CLR_DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = super::centered_list_state(
        app.selected_device,
        items.len(),
        area.height.saturating_sub(2),
    );

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

// ---------------------------------------------------------------------------
// Functions pane
// ---------------------------------------------------------------------------

pub(super) fn render_functions_pane(_app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Functions ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_DIM));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let key   = |k: &str| Span::styled(format!("[{k}]"), Style::default().fg(super::CLR_ACCENT).bold());
    let label = |s: &str| Span::styled(format!(" {s}"), Style::default().fg(Color::White));
    let dim   = |s: &str| Span::styled(s.to_string(), Style::default().fg(super::CLR_DIM));

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
        Line::from(vec![key("H"), label("Shuffle")]),
        Line::from(vec![key("V"), label("Waveform")]),
        Line::from(vec![key("Q"), label("Quit")]),
    ];

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

// ---------------------------------------------------------------------------
// P2P player pane helpers
// ---------------------------------------------------------------------------

/// Renders the "Requesting…" state — empty pulsing gauge + status line.
fn render_p2p_requesting(peer_nick: &str, _width: u16, frame: &mut Frame, area: Rect) {
    let [names_area, gauge_area, time_vol_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let info_lines = vec![
        Line::styled("Connecting…", Style::default().fg(Color::White).bold()),
        Line::styled(
            format!("Requesting from @{peer_nick}"),
            Style::default().fg(super::CLR_DIM),
        ),
        Line::default(),
    ];
    frame.render_widget(Paragraph::new(info_lines), names_area);

    // Empty gauge in accent colour to indicate activity.
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(super::CLR_ACCENT).bg(Color::DarkGray))
        .ratio(0.0);
    frame.render_widget(gauge, gauge_area);

    let status = Paragraph::new(format!("Waiting for @{peer_nick}…"))
        .style(Style::default().fg(super::CLR_DIM))
        .alignment(Alignment::Center);
    frame.render_widget(status, time_vol_area);
}

/// Renders the "Buffering…" state — download progress gauge + byte counter.
fn render_p2p_buffering(
    peer_nick: &str,
    received: u64,
    total: u64,
    stalled: bool,
    _width: u16,
    frame: &mut Frame,
    area: Rect,
) {
    let [names_area, gauge_area, time_vol_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let pct = if total > 0 { received as f64 / total as f64 } else { 0.0 };

    let info_lines = vec![
        Line::styled(
            if stalled { "⚠ Stalled" } else { "Buffering…" },
            Style::default()
                .fg(if stalled { Color::Yellow } else { Color::White })
                .bold(),
        ),
        Line::styled(
            format!("from @{peer_nick}"),
            Style::default().fg(super::CLR_DIM),
        ),
        Line::default(),
    ];
    frame.render_widget(Paragraph::new(info_lines), names_area);

    let gauge_color = if stalled { Color::Yellow } else { super::CLR_ACCENT };
    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(gauge_color).bg(Color::DarkGray))
        .ratio(pct.clamp(0.0, 1.0));
    frame.render_widget(gauge, gauge_area);

    let recv_mb = received as f64 / (1024.0 * 1024.0);
    let total_mb = total as f64 / (1024.0 * 1024.0);
    let stall_prefix = if stalled { "⚠ Stalled  " } else { "" };
    let status = Paragraph::new(format!(
        "{stall_prefix}{:.1} MB / {:.1} MB  ({:.0}%)",
        recv_mb, total_mb, pct * 100.0,
    ))
    .style(Style::default().fg(if stalled { Color::Yellow } else { super::CLR_DIM }))
    .alignment(Alignment::Center);
    frame.render_widget(status, time_vol_area);
}

// ---------------------------------------------------------------------------
// Badge helper (playlist + tag badges for library rows)
// ---------------------------------------------------------------------------

pub(super) fn build_badges<'a>(app: &'a App, path: &PathBuf) -> (Vec<Span<'a>>, usize) {
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
