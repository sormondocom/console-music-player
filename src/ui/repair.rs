//! iPod repair screen and device track browser.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;

pub(super) fn render_repair(app: &App, frame: &mut Frame, area: Rect) {
    let Some(results) = &app.repair_results else {
        let msg = Paragraph::new("No scan results. Press Esc.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(super::CLR_DIM));
        frame.render_widget(msg, area);
        return;
    };

    let issue_count = results.issue_count();
    let title = if issue_count == 0 {
        " iPod Health — All Good ".to_string()
    } else {
        format!(" iPod Repair — {issue_count} issue(s) found ")
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if issue_count == 0 { super::CLR_SUCCESS } else { super::CLR_ERROR }));

    if issue_count == 0 {
        let msg = Paragraph::new(
            "\n  No orphaned files and no missing playlist entries.\n  Your iPod database is consistent.",
        )
        .block(block)
        .style(Style::default().fg(super::CLR_SUCCESS));
        frame.render_widget(msg, area);
        return;
    }

    let mut items: Vec<ListItem> = Vec::new();

    if !results.incomplete_entries.is_empty() {
        items.push(ListItem::new(Line::styled(
            format!("  ── {} missing playlist entr(ies) ──", results.incomplete_entries.len()),
            Style::default().fg(super::CLR_DIM),
        )));
        for entry in &results.incomplete_entries {
            let label = if entry.title.is_empty() {
                entry.ipod_rel_path.clone()
            } else {
                format!("{} ({})", entry.title, entry.ipod_rel_path)
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled("  ⚠ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    super::truncate(&label, area.width as usize - 6),
                    Style::default().fg(Color::White),
                ),
            ])));
        }
    }

    if !results.orphaned_files.is_empty() {
        items.push(ListItem::new(Line::styled(
            format!("  ── {} orphaned file(s) (not in DB) ──", results.orphaned_files.len()),
            Style::default().fg(super::CLR_DIM),
        )));
        for orphan in &results.orphaned_files {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("  ○ ", Style::default().fg(super::CLR_ACCENT)),
                Span::styled(
                    super::truncate(&orphan.ipod_rel_path, area.width as usize - 6),
                    Style::default().fg(Color::White),
                ),
            ])));
        }
    }

    let mut state = super::centered_list_state(
        app.repair_selected,
        items.len(),
        area.height.saturating_sub(2),
    );

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).bold())
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}

pub(super) fn render_device_tracks(app: &App, frame: &mut Frame, area: Rect) {
    let track_count = app.device_tracks.len();
    let from_db = app.device_tracks.first().map(|t| t.from_db).unwrap_or(false);
    let source_tag = if from_db { " iTunesDB" } else { " filesystem scan" };

    let title = format!(" iPod Library — {track_count} tracks ({source_tag}) ");

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    if app.device_tracks.is_empty() {
        let msg = if from_db {
            "No tracks found in iTunesDB.\nTry transferring tracks first."
        } else {
            "No audio files found on device.\nConnect the iPod and press [D] to refresh."
        };
        let p = Paragraph::new(msg)
            .block(block)
            .alignment(Alignment::Center)
            .style(Style::default().fg(super::CLR_DIM));
        frame.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = app
        .device_tracks
        .iter()
        .map(|t| {
            let dur = if t.duration_ms > 0 {
                let secs = t.duration_ms / 1000;
                format!(" {:02}:{:02}", secs / 60, secs % 60)
            } else {
                String::new()
            };
            let fmt_badge = std::path::Path::new(&t.ipod_rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!("[{}]", e.to_uppercase()))
                .unwrap_or_default();

            let (db_marker, marker_color) = if t.from_db {
                ("●", super::CLR_SUCCESS)
            } else {
                ("○", super::CLR_DIM)
            };

            let second_col = if !t.artist.is_empty() {
                super::truncate(&t.artist, 22)
            } else {
                std::path::Path::new(&t.ipod_rel_path)
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| format!("{s}/"))
                    .unwrap_or_default()
            };

            let line = Line::from(vec![
                Span::styled(format!("{db_marker} "), Style::default().fg(marker_color)),
                Span::styled(format!("{:<6}", fmt_badge), Style::default().fg(super::CLR_ACCENT)),
                Span::styled(
                    format!(" {:<30}", super::truncate(&t.title, 28)),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(" {:<22}", second_col),
                    Style::default().fg(super::CLR_DIM),
                ),
                Span::styled(
                    if !t.album.is_empty() {
                        format!(" {:<16}", super::truncate(&t.album, 14))
                    } else {
                        String::new()
                    },
                    Style::default().fg(super::CLR_DIM),
                ),
                Span::styled(dur, Style::default().fg(super::CLR_DIM)),
            ]);
            ListItem::new(line)
        })
        .collect();

    let mut state = super::centered_list_state(
        app.device_tracks_selected,
        items.len(),
        area.height.saturating_sub(2),
    );

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(ratatui::style::Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, area, &mut state);
}
