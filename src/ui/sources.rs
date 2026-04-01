//! Sources screen (directory list).

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style, Stylize},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub(super) fn render_sources(app: &App, frame: &mut Frame, area: Rect) {
    let title = format!(
        " Sources — {} director{}",
        app.source_dirs.len(),
        if app.source_dirs.len() == 1 { "y" } else { "ies" }
    );

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    if app.source_dirs.is_empty() {
        let msg = Paragraph::new("No source directories.\nPress [A] to add one.")
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().fg(super::CLR_DIM));
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .source_dirs
        .iter()
        .map(|p| ListItem::new(p.display().to_string()))
        .collect();

    let mut state = super::centered_list_state(
        app.sources_selected,
        items.len(),
        area.height.saturating_sub(2),
    );

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}
