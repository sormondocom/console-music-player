//! Amazon Music screen — local installation detection and source import.

use ratatui::{
    layout::Rect,
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, AmazonState};

pub(super) fn render_amazon(app: &App, frame: &mut Frame, area: Rect) {
    let Some(state) = &app.amazon_state else { return };

    let block = Block::default()
        .title(" Amazon Music — Local Import ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = build_lines(state);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn build_lines(state: &AmazonState) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    if let Some(local) = &state.local {
        if local.download_dir_exists {
            lines.push(Line::from(vec![
                Span::styled("✓ ", Style::default().fg(Color::Green)),
                Span::styled(
                    "Amazon Music downloads found:",
                    Style::default().fg(Color::White).bold(),
                ),
            ]));
            lines.push(Line::from(Span::styled(
                format!("  {}", local.download_dir.display()),
                Style::default().fg(Color::Cyan),
            )));
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "  [S]  Add this folder as a library source",
                Style::default().fg(Color::DarkGray),
            )));
            if local.is_installed() {
                lines.push(Line::from(Span::styled(
                    "  [L]  Launch Amazon Music app to download more tracks",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "No Amazon Music downloads found.",
                Style::default().fg(Color::Yellow),
            )));
            lines.push(Line::default());

            if local.is_installed() {
                let app_label = if local.is_uwp {
                    "  Amazon Music (Store app) found".to_string()
                } else if let Some(exe) = &local.exe {
                    format!("  App found: {}", exe.display())
                } else {
                    "  Amazon Music found".to_string()
                };
                lines.push(Line::from(Span::styled(
                    app_label,
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    format!("  Expected downloads at: {}", local.download_dir.display()),
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "  [L]  Launch Amazon Music and download your purchases,",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "       then come back and press [S] to import them.",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "  Amazon Music desktop app not detected.",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "  Download from: amazon.com/music/unlimited/download",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines.push(Line::default());
    if !state.status.is_empty() {
        lines.push(Line::from(Span::styled(
            state.status.clone(),
            Style::default().fg(Color::White),
        )));
    }

    lines
}
