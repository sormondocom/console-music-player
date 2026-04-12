//! Remote Library screen — browse tracks shared by trusted peers.
//!
//! Layout mirrors the main library: track list on the left, player pane on
//! the right.  The player pane is the same component used by the local library
//! so buffer progress, stall warnings, and playback controls all work inline.

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub(super) fn render_remote_library(app: &App, frame: &mut Frame, area: Rect) {
    // ── Two-column layout: track list | player pane ──────────────────────────
    let [list_area, player_area] = Layout::horizontal([
        Constraint::Percentage(60),
        Constraint::Percentage(40),
    ])
    .areas(area);

    render_track_list(app, frame, list_area);
    // Re-use the same player pane renderer as the main library screen.
    super::library::render_player_pane(app, frame, player_area);
}

fn render_track_list(app: &App, frame: &mut Frame, area: Rect) {
    let count = app.remote_tracks.len();
    let title = format!(
        " ⬡ Remote Library — {} track{} ",
        count,
        if count == 1 { "" } else { "s" }
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.remote_tracks.is_empty() {
        let msg = Paragraph::new(
            "No remote tracks available.\n\
             Approved peers share their libraries automatically.\n\
             \n\
             If you just approved a peer, a catalog exchange is in progress —\n\
             watch the toast notifications in the bottom-right corner.",
        )
        .style(Style::default().fg(super::CLR_DIM))
        .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    let avail = inner.width as usize;

    let items: Vec<ListItem> = app
        .remote_tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let focused = i == app.remote_library_selected;

            let fmt_label = track.format.label();
            let dur = track
                .duration_secs
                .map(|s| format!("{:02}:{:02}", s / 60, s % 60))
                .unwrap_or_else(|| "--:--".into());

            let badge = format!("[{fmt_label}  {dur}  @{}]", track.owner_nick);
            let badge_w = badge.chars().count();
            let main_text = format!("{}  ·  {}", track.title, track.artist);
            let max_main = avail.saturating_sub(badge_w + 2);
            let main_trunc: String = main_text.chars().take(max_main).collect();
            let padding = avail.saturating_sub(main_trunc.chars().count() + badge_w);

            let main_style = if focused {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(Line::from(vec![
                Span::styled(main_trunc, main_style),
                Span::raw(" ".repeat(padding.max(1))),
                Span::styled(
                    badge,
                    Style::default()
                        .fg(super::CLR_ACCENT)
                        .add_modifier(Modifier::DIM),
                ),
            ]))
        })
        .collect();

    let total = items.len();
    let sel = app.remote_library_selected.min(total.saturating_sub(1));
    let mut list_state = super::centered_list_state(sel, total, inner.height);

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, inner, &mut list_state);
}
