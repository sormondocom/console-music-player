//! Playlists screen and playlist-conflict dialog.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub(super) fn render_playlists(app: &App, frame: &mut Frame, area: Rect) {
    let title = format!(" Playlists — {} saved ", app.playlist_names.len());

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    if app.playlist_names.is_empty() {
        let msg = Paragraph::new(
            "No playlists saved.\nSelect tracks in the library,\nthen press [W] to save one.",
        )
        .block(block)
        .alignment(Alignment::Center)
        .style(Style::default().fg(super::CLR_DIM));
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .playlist_names
        .iter()
        .map(|name| ListItem::new(name.as_str()))
        .collect();

    let mut state = super::centered_list_state(
        app.playlists_selected,
        items.len(),
        area.height.saturating_sub(2),
    );

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

pub(super) fn render_playlist_conflict(app: &App, frame: &mut Frame, area: Rect) {
    let Some(ctx) = &app.conflict_ctx else { return };

    let block = Block::default()
        .title(" Playlist Name Conflict ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ERROR));

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
            Style::default().fg(super::CLR_DIM),
        ),
        Line::styled(
            format!("  New:      {new_count} tracks"),
            Style::default().fg(super::CLR_DIM),
        ),
        Line::raw(""),
        Line::styled(
            "  [O]  Overwrite — replace with new tracks",
            Style::default().fg(super::CLR_ACCENT),
        ),
        Line::styled(
            format!("  [N]  New dated  — merged ({merged_count} unique tracks)"),
            Style::default().fg(super::CLR_ACCENT),
        ),
        Line::styled(
            "  [C]  Cancel",
            Style::default().fg(super::CLR_DIM),
        ),
    ];

    let paragraph = Paragraph::new(text).block(block);
    frame.render_widget(paragraph, area);
}
