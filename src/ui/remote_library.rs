//! Remote Library screen — browse tracks shared by trusted peers.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem},
    Frame,
};

use crate::app::App;

pub(super) fn render_remote_library(app: &App, frame: &mut Frame, area: Rect) {
    let count = app.remote_tracks.len();
    let title = format!(" ⬡ Remote Library — {} track{} ", count, if count == 1 { "" } else { "s" });

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.remote_tracks.is_empty() {
        let msg = ratatui::widgets::Paragraph::new(
            "No remote tracks available.\n\
             Trusted peers will share their libraries automatically.",
        )
        .style(Style::default().fg(super::CLR_DIM))
        .alignment(ratatui::layout::Alignment::Center);
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
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(Line::from(vec![
                Span::styled(main_trunc, main_style),
                Span::raw(" ".repeat(padding)),
                Span::styled(badge, Style::default().fg(super::CLR_ACCENT).add_modifier(Modifier::DIM)),
            ]))
        })
        .collect();

    let total = items.len();
    let sel   = app.remote_library_selected.min(total.saturating_sub(1));
    let mut list_state = super::centered_list_state(sel, total, inner.height);

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, inner, &mut list_state);
}
