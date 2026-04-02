//! File organizer screen.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, OrganizerPhase};
use crate::media::MediaItem;

pub(super) fn render_organize(app: &App, frame: &mut Frame, area: Rect) {
    let Some(state) = &app.organizer_state else { return };

    match state.phase {
        OrganizerPhase::PickGroup => render_pick_group(app, state, frame, area),
        OrganizerPhase::DestInput => render_dest_input(app, state, frame, area),
        OrganizerPhase::Running | OrganizerPhase::Done => render_log(state, frame, area),
    }
}

// ---------------------------------------------------------------------------
// Phase 1: pick a group
// ---------------------------------------------------------------------------

fn render_pick_group(
    _app: &App,
    state: &crate::app::OrganizerState,
    frame: &mut Frame,
    area: Rect,
) {
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(area);

    // ── Left: group list ──────────────────────────────────────────────────

    let group_block = Block::default()
        .title(" File Organizer — Choose Group ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner_left = group_block.inner(left_area);
    frame.render_widget(group_block, left_area);

    let group_items: Vec<ListItem> = state.groups.iter().enumerate().map(|(i, g)| {
        let style = if i == state.group_index {
            Style::default().bg(Color::DarkGray).fg(Color::White).bold()
        } else {
            Style::default()
        };
        ListItem::new(super::truncate(&g.label, inner_left.width as usize)).style(style)
    }).collect();

    let mut list_state = super::centered_list_state(
        state.group_index, group_items.len(), inner_left.height,
    );
    frame.render_stateful_widget(
        List::new(group_items),
        inner_left,
        &mut list_state,
    );

    // ── Right: tracks in selected group ──────────────────────────────────

    let track_block = Block::default()
        .title(" Tracks in Group ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_DIM));

    let inner_right = track_block.inner(right_area);
    frame.render_widget(track_block, right_area);

    if let Some(group) = state.selected_group() {
        let track_items: Vec<ListItem> = group.tracks.iter().map(|t| {
            let label = format!("{} — {}", t.display_artist(), t.display_title());
            ListItem::new(super::truncate(&label, inner_right.width as usize))
        }).collect();

        frame.render_widget(List::new(track_items), inner_right);
    }
}

// ---------------------------------------------------------------------------
// Phase 2: destination input
// ---------------------------------------------------------------------------

fn render_dest_input(
    _app: &App,
    state: &crate::app::OrganizerState,
    frame: &mut Frame,
    area: Rect,
) {
    let group = state.selected_group();
    let group_label = group.map(|g| g.label.as_str()).unwrap_or("(none)");
    let track_count = group.map(|g| g.tracks.len()).unwrap_or(0);

    let block = Block::default()
        .title(" File Organizer — Destination ")
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(super::CLR_ACCENT).bold()),
        Span::raw(state.dest_input.as_str()),
        Span::styled("▌", Style::default().fg(super::CLR_ACCENT)),
    ]);

    let content = ratatui::text::Text::from(vec![
        Line::from(vec![
            Span::styled("Group:  ", Style::default().fg(super::CLR_DIM)),
            Span::styled(group_label, Style::default().fg(Color::White).bold()),
        ]),
        Line::from(vec![
            Span::styled("Tracks: ", Style::default().fg(super::CLR_DIM)),
            Span::styled(
                format!("{track_count} file(s) will be copied, verified, then originals deleted"),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::default(),
        Line::from(Span::styled(
            "Destination folder (created if it doesn't exist, added as a source):",
            Style::default().fg(super::CLR_DIM),
        )),
        Line::default(),
        input_line,
        Line::default(),
        Line::from(vec![
            Span::styled("[Enter]", Style::default().fg(super::CLR_ACCENT).bold()),
            Span::raw(" Start move  "),
            Span::styled("[Esc]", Style::default().fg(super::CLR_DIM).bold()),
            Span::raw(" Back  "),
            Span::styled("[Ctrl+V]", Style::default().fg(super::CLR_DIM).bold()),
            Span::raw(" Paste"),
        ]),
    ]);

    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: false }), inner);
}

// ---------------------------------------------------------------------------
// Phase 3 / 4: running / done log
// ---------------------------------------------------------------------------

fn render_log(state: &crate::app::OrganizerState, frame: &mut Frame, area: Rect) {
    let title = if state.phase == OrganizerPhase::Done {
        if let Some((ok, fail)) = state.results {
            format!(" File Organizer — Done ({ok} moved, {fail} failed) ")
        } else {
            " File Organizer — Done ".into()
        }
    } else {
        " File Organizer — Moving… ".into()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if state.phase == OrganizerPhase::Done {
            Style::default().fg(super::CLR_SUCCESS)
        } else {
            Style::default().fg(super::CLR_ACCENT)
        });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = state.log.iter().map(|s| {
        let color = if s.contains('✓') {
            super::CLR_SUCCESS
        } else if s.contains('✗') || s.contains("failed") {
            super::CLR_ERROR
        } else if s.contains('⚠') {
            Color::Yellow
        } else {
            Color::White
        };
        Line::styled(s.as_str(), Style::default().fg(color))
    }).collect();

    let total = lines.len() as u16;
    let visible = inner.height;
    let scroll = if state.phase == OrganizerPhase::Running {
        // Auto-scroll to bottom while running
        total.saturating_sub(visible)
    } else {
        state.log_scroll as u16
    };

    let hint = if state.phase == OrganizerPhase::Done {
        Line::from(vec![
            Span::styled("[Esc/Q]", Style::default().fg(super::CLR_DIM).bold()),
            Span::styled(" Back to library", Style::default().fg(super::CLR_DIM)),
            Span::styled("   [↑↓/jk]", Style::default().fg(super::CLR_DIM).bold()),
            Span::styled(" Scroll log", Style::default().fg(super::CLR_DIM)),
        ])
    } else {
        Line::from(Span::styled("  Moving files… please wait.", Style::default().fg(super::CLR_DIM)))
    };

    let [log_area, hint_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(ratatui::text::Text::from(hint))
            .style(Style::default().bg(Color::DarkGray)),
        hint_area,
    );
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)),
        log_area,
    );
}
