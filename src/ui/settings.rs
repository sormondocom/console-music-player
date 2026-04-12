//! Settings screen — live-editable configuration with hot-reload.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub(super) fn render_settings(app: &App, frame: &mut Frame, area: Rect) {
    let [list_area, detail_area] = Layout::horizontal([
        Constraint::Percentage(45),
        Constraint::Percentage(55),
    ])
    .areas(area);

    render_field_list(app, frame, list_area);
    render_field_detail(app, frame, detail_area);
}

fn render_field_list(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" ⚙ Settings ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let w = inner.width as usize;

    let items: Vec<ListItem> = app
        .settings_fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let focused = i == app.settings_selected;
            let editing = focused && app.settings_editing;

            let label_w = 22usize;
            let val_w   = w.saturating_sub(label_w + 3);
            let val_str = super::truncate(&field.value, val_w);

            let label_style = Style::default().fg(super::CLR_DIM);
            let val_style = if editing {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if focused {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let cursor = if editing { "▌" } else { " " };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<label_w$}", field.label),
                    label_style,
                ),
                Span::styled(format!(" {cursor}"), Style::default().fg(Color::Yellow)),
                Span::styled(val_str, val_style),
            ]))
        })
        .collect();

    let total = items.len();
    let sel   = app.settings_selected.min(total.saturating_sub(1));
    let mut state = super::centered_list_state(sel, total, inner.height);

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, inner, &mut state);
}

fn render_field_detail(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Field Info ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_DIM));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(field) = app.settings_fields.get(app.settings_selected) else {
        return;
    };

    let editing = app.settings_editing;

    let [name_area, desc_area, val_area, hint_area] = ratatui::layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(2),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(field.label)
            .style(Style::default().fg(super::CLR_ACCENT).bold()),
        name_area,
    );

    frame.render_widget(
        Paragraph::new(field.description)
            .style(Style::default().fg(super::CLR_DIM))
            .wrap(ratatui::widgets::Wrap { trim: true }),
        desc_area,
    );

    let val_block = Block::default()
        .title(" Value ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if editing {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(super::CLR_DIM)
        });

    let val_inner = val_block.inner(val_area);
    frame.render_widget(val_block, val_area);
    frame.render_widget(
        Paragraph::new(if editing {
            format!("{}_", field.value)  // blinking cursor simulation
        } else {
            field.value.clone()
        })
        .style(if editing {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::White)
        }),
        val_inner,
    );

    let hint = if editing {
        "[Enter] Confirm  [Esc] Cancel edit"
    } else {
        "[Enter/F2] Edit  [S] Save & hot-reload all  [Esc] Back"
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(super::CLR_DIM)),
        hint_area,
    );
}
