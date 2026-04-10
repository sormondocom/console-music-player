//! Party Line screen — nominate and vote on tracks for group playback.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub(super) fn render_party_line(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" ⬡ Party Line ")
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Magenta))
        .title_style(Style::default().fg(Color::Magenta).bold());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let party = match &app.party_line {
        Some(p) => p,
        None => {
            let msg = Paragraph::new(
                "No active party line.\n\
                 Go to Remote Library and press [N] to nominate a track.",
            )
            .style(Style::default().fg(super::CLR_DIM))
            .alignment(ratatui::layout::Alignment::Center);
            frame.render_widget(msg, inner);
            return;
        }
    };

    // Split: active party (top, if any) | nominations list (rest)
    let active_height = if party.active.is_some() { 5u16 } else { 0u16 };
    let [active_area, noms_area] = Layout::vertical([
        Constraint::Length(active_height),
        Constraint::Min(0),
    ])
    .areas(inner);

    // ── Active party display ──────────────────────────────────────────────
    if let Some(active) = &party.active {
        let now = chrono::Utc::now();
        let delta = (active.start_at - now).num_milliseconds();
        let status = if delta > 0 {
            format!("Starting in {:.1}s…", delta as f64 / 1000.0)
        } else if active.started {
            "Now playing — Party Line active".into()
        } else if !active.buffer_ready {
            "Buffering…".into()
        } else {
            "Ready".into()
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("Now: ", Style::default().fg(super::CLR_DIM)),
                Span::styled(
                    format!("{} — {}", active.track.title, active.track.artist),
                    Style::default().fg(Color::Magenta).bold(),
                ),
            ]),
            Line::from(Span::styled(status, Style::default().fg(Color::Yellow))),
        ];
        frame.render_widget(Paragraph::new(lines), active_area);
    }

    // ── Nominations list ──────────────────────────────────────────────────
    if party.nominations.is_empty() {
        let msg = Paragraph::new("No active nominations.  Go to Remote Library → [N] to nominate.")
            .style(Style::default().fg(super::CLR_DIM))
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(msg, noms_area);
        return;
    }

    let avail = noms_area.width as usize;

    let items: Vec<ListItem> = party
        .nominations
        .iter()
        .enumerate()
        .map(|(i, nom)| {
            let focused = i == party.selected;
            let secs = nom.seconds_remaining();
            let vote_line = format!(
                "✓ {}  ✗ {}  ⏱ {}s — by {}",
                nom.votes_yes.len(),
                nom.votes_no.len(),
                secs,
                nom.nominated_by,
            );
            let track_label = super::truncate(
                &format!("{} — {}", nom.track.title, nom.track.artist),
                avail.saturating_sub(2),
            );

            let style = if focused {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(vec![
                Line::from(Span::styled(track_label, style)),
                Line::from(Span::styled(
                    vote_line,
                    Style::default().fg(super::CLR_DIM),
                )),
            ])
        })
        .collect();

    let total = items.len();
    let sel   = party.selected.min(total.saturating_sub(1));
    let mut list_state = super::centered_list_state(sel, total, noms_area.height);

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, noms_area, &mut list_state);
}
