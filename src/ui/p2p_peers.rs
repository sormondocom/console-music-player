//! P2P Peers screen — manage trusted/pending/rejected peers.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, Paragraph},
    Frame,
};

use crate::app::App;
use crate::p2p::trust::{NodeStatus, TrustState};

pub(super) fn render_p2p_peers(app: &App, frame: &mut Frame, area: Rect) {
    let p2p_active = app.p2p_node.is_some();

    let title = if p2p_active {
        let display = app.p2p_node.as_ref().map(|n| {
            App::p2p_display_name(&n.nickname, &n.fingerprint)
        }).unwrap_or_else(|| "?".to_string());
        format!(" ⬡ P2P Peers — {display} ")
    } else {
        " ⬡ P2P Peers (inactive) ".to_string()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(super::CLR_ACCENT));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if !p2p_active {
        let msg = Paragraph::new(
            "P2P is not active.\n\
             Press  p → 2 → p  (within 2 s) in the library to activate.",
        )
        .style(Style::default().fg(super::CLR_DIM))
        .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    // Reserve a small strip at the top for own listen addresses
    let addr_strip_height = if app.p2p_listen_addrs.is_empty() { 0 } else {
        (app.p2p_listen_addrs.len() as u16 + 2).min(inner.height / 3)
    };

    let (addr_area, peer_area) = if addr_strip_height > 0 {
        let [a, b] = Layout::vertical([
            Constraint::Length(addr_strip_height),
            Constraint::Min(0),
        ])
        .areas(inner);
        (Some(a), b)
    } else {
        (None, inner)
    };

    if let Some(aa) = addr_area {
        let addr_block = Block::default()
            .title(" Your address (share with internet peers) ")
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(super::CLR_DIM));
        let addr_inner = addr_block.inner(aa);
        frame.render_widget(addr_block, aa);

        let lines: Vec<Line> = app.p2p_listen_addrs.iter().map(|a| {
            Line::from(Span::styled(
                super::truncate(a, addr_inner.width.saturating_sub(2) as usize),
                Style::default().fg(super::CLR_ACCENT),
            ))
        }).collect();
        frame.render_widget(Paragraph::new(lines), addr_inner);
    }

    let inner = peer_area;

    if app.p2p_peer_list.is_empty() {
        let msg = Paragraph::new("No peers discovered yet.\nDiscovery may take a few seconds.")
            .style(Style::default().fg(super::CLR_DIM))
            .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    let avail = inner.width as usize;

    let items: Vec<ListItem> = app
        .p2p_peer_list
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let focused = i == app.p2p_peers_selected;

            let (trust_badge, trust_color) = match info.trust {
                TrustState::Trusted  => ("[trusted]",  Color::Green),
                TrustState::Pending  => ("[pending]",  Color::Yellow),
                TrustState::Deferred => ("[deferred]", Color::Cyan),
                TrustState::Rejected => ("[rejected]", Color::Red),
            };

            let status_icon = match info.status {
                NodeStatus::Online    => "● ",
                NodeStatus::Deferring => "◐ ",
                NodeStatus::Offline   => "○ ",
            };
            let status_color = match info.status {
                NodeStatus::Online    => Color::Green,
                NodeStatus::Deferring => Color::Yellow,
                NodeStatus::Offline   => super::CLR_DIM,
            };

            let display_name = App::p2p_display_name(&info.nickname, &info.fingerprint);
            let nick = super::truncate(&display_name, avail.saturating_sub(16));

            let main_style = if focused {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(Line::from(vec![
                Span::styled(status_icon, Style::default().fg(status_color)),
                Span::styled(nick,        main_style),
                Span::raw("  "),
                Span::styled(trust_badge, Style::default().fg(trust_color).bold()),
            ]))
        })
        .collect();

    let total = items.len();
    let sel   = app.p2p_peers_selected.min(total.saturating_sub(1));
    let mut list_state = super::centered_list_state(sel, total, inner.height);

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, inner, &mut list_state);
}
