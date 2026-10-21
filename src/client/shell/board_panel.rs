//! fork: the board section under the agents panel (`crate::board`).
//!
//! Every agent's asks and links are listed under its name, so the user sees
//! everything waiting on them in one place; the focused agent's todos follow.
//! Clicking a name or an ask focuses that pane, and clicking a link (with or
//! without ctrl) opens it.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use super::render::{ordered_agent_pane_ids, put_text};
use super::*;
use crate::board::{AskKind, TodoState};

/// What a click on a board line does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::client::shell) enum BoardTarget {
    FocusPane(String),
    OpenUrl(String),
}

struct BoardLine {
    spans: Vec<Span<'static>>,
    target: Option<BoardTarget>,
}

/// The agents panel keeps at least this many rows before the board takes any.
const MIN_AGENT_ROWS: u16 = 6;

/// Splits the agents area into the agents panel and the board below it. The
/// board takes only the rows it needs, and at most half of the area.
pub(super) fn split_board_area(
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
) -> (Rect, Rect) {
    let lines = board_lines(snapshot, config).len() as u16;
    if lines == 0 || area.height < MIN_AGENT_ROWS * 2 {
        return (area, Rect::default());
    }
    // Header (rule and title), the lines, and a free last row for the
    // sidebar's collapse button.
    let height = (lines + 3).min(area.height / 2);
    let agents = Rect::new(area.x, area.y, area.width, area.height - height);
    let board = Rect::new(area.x, agents.bottom(), area.width, height);
    (agents, board)
}

pub(super) fn render_board_panel(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    if area.height < 4 {
        return;
    }
    let palette = &config.palette;
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(palette.surface_dim),
    );
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        " board",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );
    let lines = board_lines(snapshot, config);
    let rows = area.height.saturating_sub(3) as usize;
    let hidden = lines.len().saturating_sub(rows);
    let shown = if hidden > 0 {
        rows.saturating_sub(1)
    } else {
        rows
    };
    let mut y = area.y + 2;
    for line in lines.into_iter().take(shown) {
        let rect = Rect::new(area.x, y, area.width, 1);
        Paragraph::new(Line::from(line.spans)).render(rect, buffer);
        if let Some(target) = line.target {
            hits.board.push((rect, target));
        }
        y += 1;
    }
    if hidden > 0 {
        put_text(
            buffer,
            area.x,
            y,
            area.width,
            &format!("   … 他 {} 件", hidden + 1),
            Style::default().fg(palette.overlay0),
        );
    }
}

fn board_lines(snapshot: &ClientShellSnapshot, config: &ClientShellConfig) -> Vec<BoardLine> {
    let palette = &config.palette;
    let mut lines = Vec::new();
    let agents = ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .filter_map(|pane_id| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
        })
        .collect::<Vec<_>>();
    for agent in &agents {
        let board = &agent.board;
        if board.asks.is_empty() && board.links.is_empty() {
            continue;
        }
        lines.push(agent_heading(snapshot, agent, config));
        for ask in &board.asks {
            let (mark, color) = match ask.kind {
                AskKind::Do => ("!", palette.peach),
                _ => ("?", palette.red),
            };
            lines.push(BoardLine {
                spans: vec![
                    Span::raw("   "),
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(ask.text.clone(), Style::default().fg(palette.text)),
                ],
                target: Some(BoardTarget::FocusPane(agent.pane_id.clone())),
            });
        }
        for link in &board.links {
            lines.push(BoardLine {
                spans: vec![
                    Span::raw("   "),
                    Span::styled("↗ ", Style::default().fg(palette.accent)),
                    Span::styled(
                        link.label.clone(),
                        Style::default()
                            .fg(palette.accent)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ],
                target: Some(BoardTarget::OpenUrl(link.url.clone())),
            });
        }
    }
    let focused = agents
        .iter()
        .find(|agent| snapshot.focused_pane_id.as_deref() == Some(agent.pane_id.as_str()))
        .filter(|agent| !agent.board.todos.is_empty());
    if let Some(agent) = focused {
        let todos = &agent.board.todos;
        let finished = todos
            .iter()
            .filter(|todo| todo.state == TodoState::Done)
            .count();
        lines.push(BoardLine {
            spans: vec![
                Span::styled(
                    " todo ",
                    Style::default()
                        .fg(palette.overlay0)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{finished}/{}", todos.len()),
                    Style::default().fg(palette.overlay0),
                ),
            ],
            target: None,
        });
        for todo in todos {
            let (mark, mark_style, text_style) = match todo.state {
                TodoState::Done => (
                    "✓",
                    Style::default().fg(palette.green),
                    Style::default()
                        .fg(palette.overlay0)
                        .add_modifier(Modifier::DIM),
                ),
                TodoState::InProgress => (
                    "▶",
                    Style::default().fg(palette.yellow),
                    Style::default()
                        .fg(palette.text)
                        .add_modifier(Modifier::BOLD),
                ),
                _ => (
                    "○",
                    Style::default().fg(palette.overlay0),
                    Style::default().fg(palette.subtext0),
                ),
            };
            lines.push(BoardLine {
                spans: vec![
                    Span::raw("   "),
                    Span::styled(format!("{mark} "), mark_style),
                    Span::styled(todo.text.clone(), text_style),
                ],
                target: None,
            });
        }
    }
    lines
}

fn agent_heading(
    snapshot: &ClientShellSnapshot,
    agent: &crate::protocol::ClientShellAgent,
    config: &ClientShellConfig,
) -> BoardLine {
    let palette = &config.palette;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)
        .map(|workspace| workspace.label.as_str())
        .unwrap_or_default();
    let agent_label = agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.title.as_deref())
        .or(agent.agent.as_deref())
        .unwrap_or_default();
    let name_style = if agent.focused {
        Style::default()
            .fg(palette.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.subtext0)
            .add_modifier(Modifier::BOLD)
    };
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(workspace.to_string(), name_style),
    ];
    if !agent_label.is_empty() {
        spans.push(Span::styled(
            format!(" · {agent_label}"),
            Style::default().fg(palette.overlay0),
        ));
    }
    BoardLine {
        spans,
        target: Some(BoardTarget::FocusPane(agent.pane_id.clone())),
    }
}
