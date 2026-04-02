//! Amazon Music easter egg screen.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::app::{AmazonFocus, AmazonOverlay, AmazonState, App};
use crate::media::MediaItem;

pub(super) fn render_amazon(app: &App, frame: &mut Frame, area: Rect) {
    let Some(state) = &app.amazon_state else { return };

    if let Some(ov) = &state.overlay {
        render_amazon_overlay(app, ov, frame, area);
        return;
    }

    // Diagnostic log view — shown when [?] is pressed and errors exist.
    if state.show_diagnostic {
        render_diagnostic_log(state, frame, area);
        return;
    }

    let [left_area, right_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(area);

    // ── Left: Amazon catalog ────────────────────────────────────────────────

    let catalog_focused = state.focus == AmazonFocus::Catalog;
    let catalog_border_style = if catalog_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(super::CLR_DIM)
    };

    let catalog_title = if state.loading {
        " Amazon Music  [loading…] "
    } else {
        " Amazon Music "
    };

    let catalog_block = Block::default()
        .title(catalog_title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(catalog_border_style);

    let inner_left = catalog_block.inner(left_area);
    frame.render_widget(catalog_block, left_area);

    let [catalog_list_area, catalog_status_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(inner_left);

    if !state.status.is_empty() {
        let status_line = Line::from(vec![Span::styled(
            state.status.as_str(),
            Style::default().fg(super::CLR_DIM),
        )]);
        frame.render_widget(Paragraph::new(status_line), catalog_status_area);
    }

    let items: Vec<ListItem> = state
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let indicator = if state.completed.contains(&track.asin) {
                Span::styled("✓ ", Style::default().fg(super::CLR_SUCCESS))
            } else if state.downloading.contains(&track.asin) {
                let pct = state.progress.get(&track.asin).map(|(b, t)| {
                    t.map(|total| if total > 0 { *b * 100 / total } else { 0 })
                        .unwrap_or(0)
                });
                let label = format!("↓{:3}% ", pct.unwrap_or(0));
                Span::styled(label, Style::default().fg(Color::Cyan))
            } else {
                Span::styled("◇ ", Style::default().fg(super::CLR_DIM))
            };

            let text = Line::from(vec![indicator, Span::raw(track.display_line())]);

            let style = if i == state.catalog_index && catalog_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if i == state.catalog_index {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let mut list_state = if !state.tracks.is_empty() {
        super::centered_list_state(state.catalog_index, state.tracks.len(), catalog_list_area.height)
    } else {
        ListState::default()
    };
    frame.render_stateful_widget(List::new(items), catalog_list_area, &mut list_state);

    // ── Right: local library ────────────────────────────────────────────────

    let local_focused = state.focus == AmazonFocus::Local;
    let local_border_style = if local_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(super::CLR_DIM)
    };

    let local_block = Block::default()
        .title(" Local Library ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(local_border_style);

    let inner_right = local_block.inner(right_area);
    frame.render_widget(local_block, right_area);

    let local_items: Vec<ListItem> = app
        .library
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let label = format!("{} — {}", track.display_artist(), track.display_title());
            let style = if i == state.local_index && local_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if i == state.local_index {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            ListItem::new(label).style(style)
        })
        .collect();

    let local_count = local_items.len();
    let local_sel = state.local_index.min(local_count.saturating_sub(1));
    let mut local_state = if local_count > 0 {
        super::centered_list_state(local_sel, local_count, inner_right.height)
    } else {
        ListState::default()
    };
    frame.render_stateful_widget(List::new(local_items), inner_right, &mut local_state);
}

fn render_amazon_overlay(app: &App, overlay: &AmazonOverlay, frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        area,
    );

    let cookie_prefilled = !app.input_buffer.is_empty();
    let (title, prompt) = match overlay {
        AmazonOverlay::CookieInput => (
            " Amazon Music — Session Cookie ",
            if cookie_prefilled {
                "Cookie pre-filled from last session — press Enter to reuse or Ctrl+V to paste a fresh one.\n\
                 Cookies expire; if the catalog fails with a 404 or HTML error, paste a new cookie.\n\
                 How: music.amazon.com → F12 → Network → any request → Headers → right-click \"cookie:\" → Copy value."
            } else {
                "Paste your amazon.com request cookie (Ctrl+V) — required every session.\n\
                 How: open music.amazon.com → F12 → Network tab → click any request\n\
                 → Headers → Request Headers → right-click \"cookie:\" value → Copy value."
            },
        ),
        AmazonOverlay::DirInput => (
            " Amazon Music — Download Directory ",
            "Enter or paste (Ctrl+V) the folder where MP3 downloads will be saved:",
        ),
    };

    let width = (area.width as f32 * 0.70) as u16;
    let height = 12u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect { x, y, width, height };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(box_area);
    frame.render_widget(Clear, box_area);
    frame.render_widget(block, box_area);

    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Yellow).bold()),
        Span::raw(app.input_buffer.as_str()),
        Span::styled("▌", Style::default().fg(Color::Yellow)),
    ]);

    let ctrl = Line::from(vec![
        Span::styled("[Enter]", Style::default().fg(super::CLR_DIM).bold()),
        Span::raw(" Confirm  "),
        Span::styled("[Esc]", Style::default().fg(super::CLR_DIM).bold()),
        Span::raw(" Cancel"),
    ]);

    let content = Text::from(vec![
        Line::from(Span::styled(prompt, Style::default().fg(Color::Gray))),
        Line::default(),
        input_line,
        Line::default(),
        ctrl,
    ]);

    frame.render_widget(Paragraph::new(content).wrap(Wrap { trim: false }), inner);
}

// ---------------------------------------------------------------------------
// Diagnostic log view
// ---------------------------------------------------------------------------

fn render_diagnostic_log(state: &AmazonState, frame: &mut Frame, area: Rect) {
    let count = state.diagnostic_log.len();
    let title = format!(" Amazon Diagnostic Log — {count} record{} ", if count == 1 { "" } else { "s" });

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.diagnostic_log.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("No diagnostics recorded.", Style::default().fg(super::CLR_DIM))),
            inner,
        );
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    for (idx, diag) in state.diagnostic_log.iter().enumerate() {
        // ── Record separator ───────────────────────────────────────────────
        lines.push(Line::from(Span::styled(
            format!("─── Record {} ─────────────────────────────────────────────", idx + 1),
            Style::default().fg(Color::Red),
        )));

        // Operation + request line
        lines.push(Line::from(vec![
            Span::styled("Operation : ", Style::default().fg(super::CLR_DIM)),
            Span::styled(&diag.operation, Style::default().fg(Color::White).bold()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Request   : ", Style::default().fg(super::CLR_DIM)),
            Span::styled(&diag.request_line, Style::default().fg(Color::White)),
        ]));

        // Request headers
        lines.push(Line::from(Span::styled("Req Headers:", Style::default().fg(super::CLR_DIM))));
        for (k, v) in &diag.request_headers {
            lines.push(Line::from(Span::styled(
                format!("  {k}: {v}"),
                Style::default().fg(super::CLR_DIM),
            )));
        }

        // HTTP status
        let status_color = if diag.status < 300 { super::CLR_SUCCESS } else { Color::Red };
        lines.push(Line::from(vec![
            Span::styled("Status    : ", Style::default().fg(super::CLR_DIM)),
            Span::styled(diag.status.to_string(), Style::default().fg(status_color).bold()),
        ]));

        // Response headers
        lines.push(Line::from(Span::styled("Resp Headers:", Style::default().fg(super::CLR_DIM))));
        for (k, v) in &diag.response_headers {
            // Highlight Content-Type — it's the first clue for HTML vs JSON
            let style = if k.eq_ignore_ascii_case("content-type") {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(super::CLR_DIM)
            };
            lines.push(Line::from(Span::styled(format!("  {k}: {v}"), style)));
        }

        // Context / analysis note
        if let Some(ctx) = &diag.context {
            lines.push(Line::from(Span::styled("Note:", Style::default().fg(Color::Yellow))));
            for ctx_line in ctx.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {ctx_line}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }

        // Full response body
        lines.push(Line::from(Span::styled(
            format!("Body ({} bytes):", diag.body.len()),
            Style::default().fg(super::CLR_DIM),
        )));
        for body_line in diag.body.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {body_line}"),
                Style::default().fg(Color::Gray),
            )));
        }
        // If body had no newlines (e.g. minified HTML/JSON), it shows as one line above —
        // add a blank separator anyway.
        lines.push(Line::default());
    }

    // Hint line at the bottom
    let hint = Line::from(vec![
        Span::styled("[?/Esc]", Style::default().fg(super::CLR_DIM).bold()),
        Span::styled(" Close  ", Style::default().fg(super::CLR_DIM)),
        Span::styled("[↑↓/jk]", Style::default().fg(super::CLR_DIM).bold()),
        Span::styled(" Scroll  Body is untruncated — scroll to see full response.", Style::default().fg(super::CLR_DIM)),
    ]);

    let [log_area, hint_area] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Min(0),
        ratatui::layout::Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(hint_area_widget(hint), hint_area);

    // Scrollable paragraph — scroll from the bottom so the latest record is
    // visible first.
    let total_lines = lines.len() as u16;
    let visible = log_area.height;
    let scroll_offset = total_lines.saturating_sub(visible);

    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll_offset, 0))
            .wrap(Wrap { trim: false }),
        log_area,
    );
}

fn hint_area_widget(line: Line<'_>) -> Paragraph<'_> {
    Paragraph::new(line).style(Style::default().bg(Color::DarkGray))
}
