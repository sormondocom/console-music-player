//! Transfer log screen.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::App;

pub(super) fn render_transfer(app: &App, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Transfer Log ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let lines: Vec<Line> = app
        .transfer_log
        .iter()
        .map(|s| {
            let color = if s.contains('✓') {
                super::CLR_SUCCESS
            } else if s.contains('✗') {
                super::CLR_ERROR
            } else {
                Color::White
            };
            Line::styled(s.as_str(), Style::default().fg(color))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        area,
    );
}
