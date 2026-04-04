//! Amazon Music screen — local installation detection, source import, and CDP automation.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, AmazonState};

pub(super) fn render_amazon(app: &App, frame: &mut Frame, area: Rect) {
    let Some(state) = &app.amazon_state else { return };

    // If the CDP log has content, split vertically: detection pane on top,
    // log pane on the bottom half.
    let (info_area, log_area) = if state.cdp_log.is_empty() {
        (area, None)
    } else {
        let log_height = (state.cdp_log.len() as u16 + 2).min(area.height / 2).max(4);
        let chunks = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(log_height),
        ])
        .split(area);
        (chunks[0], Some(chunks[1]))
    };

    // ── Detection / info pane ───────────────────────────────────────────────
    let block = Block::default()
        .title(" Amazon Music — Local Import ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(info_area);
    frame.render_widget(block, info_area);
    let lines = build_info_lines(state);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    // ── CDP log pane ────────────────────────────────────────────────────────
    if let Some(log_rect) = log_area {
        let title = if state.cdp_running {
            " CDP Automation — running… "
        } else {
            " CDP Automation Log "
        };
        let log_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if state.cdp_running {
                Color::Cyan
            } else {
                Color::DarkGray
            }));

        let log_inner = log_block.inner(log_rect);
        frame.render_widget(log_block, log_rect);

        let visible_h = log_inner.height as usize;
        let total = state.cdp_log.len();
        // cdp_log_scroll is "lines from the bottom"; 0 = stay at bottom
        let scroll = state.cdp_log_scroll.min(total.saturating_sub(visible_h));
        let start = total.saturating_sub(visible_h + scroll);
        let log_lines: Vec<Line<'static>> = state.cdp_log[start..]
            .iter()
            .take(visible_h)
            .map(|l| {
                let (color, text) = if l.starts_with('✗') {
                    (Color::Red, l.clone())
                } else if l.starts_with("──") {
                    (Color::DarkGray, l.clone())
                } else if l.starts_with('✓') || l.starts_with("Clicked:") {
                    (Color::Green, l.clone())
                } else if l.starts_with('⚠') {
                    (Color::Yellow, l.clone())
                } else {
                    (Color::White, l.clone())
                };
                Line::from(Span::styled(text, Style::default().fg(color)))
            })
            .collect();

        frame.render_widget(Paragraph::new(log_lines), log_inner);
    }
}

fn build_info_lines(state: &AmazonState) -> Vec<Line<'static>> {
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

                if local.is_uwp {
                    lines.push(Line::from(Span::styled(
                        "  [L]  Launch Amazon Music (Store app) — manually download your purchases,",
                        Style::default().fg(Color::DarkGray),
                    )));
                    lines.push(Line::from(Span::styled(
                        "       then come back and press [S] to import them.",
                        Style::default().fg(Color::DarkGray),
                    )));
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        "  [D]  Auto-download via CDP  (requires Win32 installer, not Store app)",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    lines.push(Line::from(Span::styled(
                        "  [L]  Launch Amazon Music and download your purchases,",
                        Style::default().fg(Color::DarkGray),
                    )));
                    lines.push(Line::from(Span::styled(
                        "       then come back and press [S] to import them.",
                        Style::default().fg(Color::DarkGray),
                    )));
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        "  [D]  Auto-download — launches app with debug port and triggers downloads",
                        Style::default().fg(Color::Cyan),
                    )));
                }
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
