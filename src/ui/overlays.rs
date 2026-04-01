//! Shared overlay renderers: edit, tag-edit, input, and search.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, EditState, SearchState, EDIT_FIELD_LABELS};
use crate::media::MediaItem;

// ---------------------------------------------------------------------------
// Track metadata editor
// ---------------------------------------------------------------------------

pub(super) fn render_edit_overlay(state: &EditState, frame: &mut Frame, parent: Rect) {
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
        .border_style(Style::default().fg(super::CLR_SELECTED));

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
    let val_width = inner.width.saturating_sub(8) as usize;

    for (i, (label, value)) in EDIT_FIELD_LABELS.iter().zip(state.fields.iter()).enumerate() {
        let focused = i == state.focused_field;
        let display = super::truncate(value, val_width.saturating_sub(1));
        let cursor = if focused { "_" } else { "" };

        let line = Line::from(vec![
            Span::styled(*label, Style::default().fg(super::CLR_DIM)),
            Span::styled(
                format!("{display}{cursor}"),
                if focused {
                    Style::default().fg(super::CLR_SELECTED).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]);
        frame.render_widget(Paragraph::new(line), field_areas[i]);
    }

    frame.render_widget(
        Paragraph::new("[Tab/↑↓] Next field  [Enter] Save  [Esc] Cancel")
            .style(Style::default().fg(super::CLR_DIM)),
        hint_area,
    );
}

// ---------------------------------------------------------------------------
// Generic single-line input overlay (used for Save Playlist, Add Source)
// ---------------------------------------------------------------------------

pub(super) fn render_input_overlay(title: &str, buffer: &str, frame: &mut Frame, parent: Rect) {
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
                .border_style(Style::default().fg(super::CLR_SELECTED)),
        )
        .style(Style::default().fg(Color::White));

    frame.render_widget(paragraph, popup);
}

// ---------------------------------------------------------------------------
// Tag editor overlay
// ---------------------------------------------------------------------------

pub(super) fn render_tag_edit_overlay(app: &App, frame: &mut Frame, parent: Rect) {
    let Some(state) = &app.tag_edit_state else { return };

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

    let name_line = Line::from(Span::styled(
        super::truncate(&state.display_name, inner.width as usize),
        Style::default().fg(Color::White).bold(),
    ));

    let current_tags = app.tag_store.tags_for(&state.path);
    let tag_spans: Vec<Span> = if current_tags.is_empty() {
        vec![Span::styled("  (none)", Style::default().fg(super::CLR_DIM))]
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
        Span::styled("Tags: ", Style::default().fg(super::CLR_DIM)),
    ]);
    let tags_line = Line::from(tag_spans);

    let input_label = Line::from(Span::styled(
        "Edit (comma-separated):",
        Style::default().fg(super::CLR_DIM),
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

    let ctrl = Line::from(vec![
        Span::styled("[Enter]", Style::default().fg(Color::Magenta).bold()),
        Span::raw(" Save  "),
        Span::styled("[Esc]", Style::default().fg(super::CLR_DIM).bold()),
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

// ---------------------------------------------------------------------------
// Live search overlay
// ---------------------------------------------------------------------------

pub(super) fn render_search_overlay(state: &SearchState, frame: &mut Frame, area: Rect) {
    let max_results = 16usize;
    let result_rows = state.results.len().min(max_results) as u16;
    let width = (area.width as f32 * 0.80) as u16;
    let height = (result_rows + 5).max(7).min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect { x, y, width, height };

    let result_count = state.results.len();
    let title = if state.query.is_empty() {
        " Search ".to_string()
    } else if result_count == 0 {
        " Search — no results ".to_string()
    } else {
        format!(" Search — {result_count} result{} ", if result_count == 1 { "" } else { "s" })
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(box_area);
    frame.render_widget(Clear, box_area);
    frame.render_widget(block, box_area);

    let [input_area, results_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let input_line = Line::from(vec![
        Span::styled("/", Style::default().fg(super::CLR_ACCENT).bold()),
        Span::raw(" "),
        Span::raw(state.query.as_str()),
        Span::styled("▌", Style::default().fg(super::CLR_ACCENT)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    if state.results.is_empty() {
        if !state.query.trim().is_empty() {
            frame.render_widget(
                Paragraph::new(Span::styled("No matches found.", Style::default().fg(super::CLR_DIM))),
                results_area,
            );
        }
        return;
    }

    let avail_width = results_area.width as usize;

    let items: Vec<ListItem> = state
        .results
        .iter()
        .take(max_results)
        .enumerate()
        .map(|(i, result)| {
            let selected = i == state.selected;
            let badge = format!("[{}]", result.matched_fields.join(" · "));
            let badge_width = badge.chars().count();
            let main_text = format!(
                "{} — {}",
                result.track.display_artist(),
                result.track.display_title()
            );
            let max_main = avail_width.saturating_sub(badge_width + 2);
            let main_truncated: String = main_text.chars().take(max_main).collect();
            let padding = avail_width.saturating_sub(main_truncated.chars().count() + badge_width);

            let main_style = if selected {
                Style::default().fg(Color::White).bg(Color::DarkGray).bold()
            } else {
                Style::default()
            };
            let badge_style = if selected {
                Style::default().fg(super::CLR_ACCENT).bg(Color::DarkGray).bold()
            } else {
                Style::default().fg(super::CLR_ACCENT).add_modifier(Modifier::DIM)
            };

            ListItem::new(Line::from(vec![
                Span::styled(main_truncated, main_style),
                Span::styled(" ".repeat(padding), main_style),
                Span::styled(badge, badge_style),
            ]))
        })
        .collect();

    let display_count = state.results.len().min(max_results);
    let sel = state.selected.min(display_count.saturating_sub(1));
    let mut list_state = super::centered_list_state(sel, display_count, results_area.height);
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::default().bg(Color::DarkGray)),
        results_area,
        &mut list_state,
    );
}
