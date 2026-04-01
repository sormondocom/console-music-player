//! Deduplication screen.

use humansize::{format_size, DECIMAL};
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, DedupFocus};
use crate::library::dedup::{DedupAction, DuplicateKind};

pub(super) fn render_dedup(app: &App, frame: &mut Frame, area: Rect) {
    let Some(state) = &app.dedup_state else { return };

    let [left, right] = ratatui::layout::Layout::horizontal([
        Constraint::Percentage(35),
        Constraint::Percentage(65),
    ])
    .areas(area);

    // ── Left: group list ────────────────────────────────────────────────────
    let group_items: Vec<ListItem> = state
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let kind_tag = match g.kind {
                DuplicateKind::ExactContent  => "=",
                DuplicateKind::MetadataMatch => "~",
            };
            let title = g
                .candidates
                .first()
                .map(|c| super::truncate(&c.track.title, 22))
                .unwrap_or_default();
            let del_count = state.actions.get(i)
                .map(|acts| acts.iter().filter(|&&a| a == DedupAction::Delete).count())
                .unwrap_or(0);
            let del_tag = if del_count > 0 { format!(" -{del_count}") } else { String::new() };

            let text = format!("{kind_tag} {title}{del_tag}");
            let style = if i == state.group_index {
                if state.focus == DedupFocus::Groups {
                    Style::default().fg(super::CLR_SELECTED).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                }
            } else {
                Style::default().fg(super::CLR_DIM)
            };
            ListItem::new(text).style(style)
        })
        .collect();

    let left_border = if state.focus == DedupFocus::Groups { super::CLR_SELECTED } else { super::CLR_DIM };
    let groups_widget = List::new(group_items).block(
        Block::default()
            .title(format!(
                " Duplicates — {} group(s)  [=]exact [~]meta ",
                state.group_count()
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(left_border)),
    );

    let mut list_state = super::centered_list_state(
        state.group_index,
        state.group_count(),
        left.height.saturating_sub(2),
    );
    frame.render_stateful_widget(groups_widget, left, &mut list_state);

    // ── Right: candidate detail ──────────────────────────────────────────────
    if let (Some(group), Some(actions)) = (state.focused_group(), state.focused_actions()) {
        let kind_label = match group.kind {
            DuplicateKind::ExactContent  => "Exact content match (identical bytes)",
            DuplicateKind::MetadataMatch => "Metadata match (same title + artist)",
        };

        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                Span::styled("  Kind: ", Style::default().fg(super::CLR_DIM)),
                Span::styled(kind_label, Style::default().fg(super::CLR_ACCENT)),
            ]),
            Line::raw(""),
        ];

        for (ci, (candidate, &action)) in
            group.candidates.iter().zip(actions.iter()).enumerate()
        {
            let focused = ci == state.candidate_index && state.focus == DedupFocus::Candidates;
            let action_style = match action {
                DedupAction::Keep      => Style::default().fg(super::CLR_SUCCESS).bold(),
                DedupAction::Delete    => Style::default().fg(super::CLR_ERROR).bold(),
                DedupAction::Undecided => Style::default().fg(super::CLR_DIM),
            };
            let selector = if focused { "▶ " } else { "  " };

            lines.push(Line::from(vec![
                Span::raw(selector),
                Span::styled(format!("[{}]", action.label()), action_style),
                Span::raw(format!("  #{}", ci + 1)),
            ]));

            let t = &candidate.track;
            let path_str = super::truncate(&t.path.to_string_lossy(), right.width as usize - 6);
            lines.push(Line::from(Span::styled(
                format!("     {path_str}"),
                Style::default().fg(Color::White),
            )));

            let mut meta = Vec::new();
            if !t.title.is_empty() { meta.push(t.title.clone()); }
            if !t.artist.is_empty() { meta.push(t.artist.clone()); }
            if !t.album.is_empty() {
                let a = match t.year {
                    Some(y) => format!("{} ({})", t.album, y),
                    None    => t.album.clone(),
                };
                meta.push(a);
            }
            if !meta.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("     {}", meta.join(" · ")),
                    Style::default().fg(super::CLR_DIM),
                )));
            }

            let mut info = Vec::new();
            info.push(format_size(t.file_size, DECIMAL));
            if let Some(d) = t.duration_secs {
                info.push(format!("{}:{:02}", d / 60, d % 60));
            }
            if let Some(br) = t.bitrate_kbps {
                info.push(format!("{br} kbps"));
            }
            if let Some(cs) = candidate.checksum {
                info.push(format!("fp:{cs:016x}"));
            }
            lines.push(Line::from(Span::styled(
                format!("     {}", info.join("  ")),
                Style::default().fg(super::CLR_DIM),
            )));
            lines.push(Line::raw(""));
        }

        let to_delete = actions.iter().filter(|&&a| a == DedupAction::Delete).count();
        let summary = format!(
            "  Group {}/{} — {} to delete total across all groups",
            state.group_index + 1,
            state.group_count(),
            state.to_delete_count(),
        );
        lines.push(Line::from(Span::styled(summary, Style::default().fg(super::CLR_ACCENT))));
        let _ = to_delete;

        let right_border = if state.focus == DedupFocus::Candidates { super::CLR_SELECTED } else { super::CLR_DIM };
        let detail = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Candidates — [Space] cycle action  [A] auto-suggest ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(right_border)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(detail, right);
    }
}
