//! fork: the agents' boards (`crate::board`).
//!
//! The sidebar lists every agent's asks under its name, so the user sees
//! everything waiting on them in one place; clicking one focuses that pane.
//! A column right of the panes shows the focused agent's whole board (asks,
//! links, todos), and clicking a link (with or without ctrl) opens it.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthChar;

use super::render::{display_width, ordered_agent_pane_ids, put_text};
use super::*;
use crate::board::{AskKind, PaneBoard, TodoState};
use crate::protocol::ClientShellAgent;

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

/// The agents panel keeps at least this many rows before the asks take any.
const MIN_AGENT_ROWS: u16 = 6;
/// Width of the board column, including its left border.
const BOARD_COLUMN_WIDTH: u16 = 34;

/// The board column shows once any agent has a board. It does not follow
/// focus, so switching panes never resizes them.
pub(super) fn board_column_width(snapshot: Option<&ClientShellSnapshot>) -> u16 {
    if snapshot.is_some_and(|snapshot| snapshot.agents.iter().any(|agent| !agent.board.is_empty()))
    {
        BOARD_COLUMN_WIDTH
    } else {
        0
    }
}

/// Splits the agents area into the agents panel and the asks below it. The
/// asks take only the rows they need, and at most half of the area.
pub(super) fn split_board_area(
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
) -> (Rect, Rect) {
    let lines = ask_lines(snapshot, config).len() as u16;
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
        " あなたへ",
        section_style(config),
    );
    let lines = ask_lines(snapshot, config);
    let rows = area.height.saturating_sub(3) as usize;
    render_lines(
        buffer,
        Rect::new(area.x, area.y + 2, area.width, rows as u16),
        lines,
        config,
        hits,
    );
}

/// Draws the focused agent's board in the column right of the panes.
pub(super) fn render_board_column(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    if area.width < 4 || area.height == 0 {
        return;
    }
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.sidebar_bg));
    for y in area.y..area.bottom() {
        if let Some(cell) = buffer.cell_mut((area.x, y)) {
            cell.set_symbol("│");
            cell.set_style(Style::default().fg(palette.surface_dim));
        }
    }
    let inner = Rect::new(area.x + 1, area.y, area.width - 1, area.height);
    put_text(
        buffer,
        inner.x,
        inner.y,
        inner.width,
        " board",
        section_style(config),
    );
    let agent = snapshot.focused_pane_id.as_deref().and_then(|pane_id| {
        snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
    });
    let mut lines = Vec::new();
    if let Some(agent) = agent {
        lines.push(agent_heading(snapshot, agent, config));
        lines.push(blank());
    }
    match agent
        .map(|agent| &agent.board)
        .filter(|board| !board.is_empty())
    {
        Some(board) => lines.extend(column_lines(board, inner.width, config)),
        None => lines.push(BoardLine {
            spans: vec![Span::styled(
                " このペインのボードはありません",
                Style::default().fg(palette.overlay0),
            )],
            target: None,
        }),
    }
    render_lines(
        buffer,
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(2),
        ),
        lines,
        config,
        hits,
    );
}

/// Renders lines top-down, ending with "… 他 N 行" when they do not fit.
fn render_lines(
    buffer: &mut Buffer,
    area: Rect,
    lines: Vec<BoardLine>,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let rows = area.height as usize;
    let hidden = lines.len().saturating_sub(rows);
    let shown = if hidden > 0 {
        rows.saturating_sub(1)
    } else {
        rows
    };
    let mut y = area.y;
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
            &format!("   … 他 {} 行", hidden + 1),
            Style::default().fg(config.palette.overlay0),
        );
    }
}

/// Every agent's asks, under its name, for the sidebar.
fn ask_lines(snapshot: &ClientShellSnapshot, config: &ClientShellConfig) -> Vec<BoardLine> {
    let mut lines = Vec::new();
    for pane_id in ordered_agent_pane_ids(snapshot, config.agent_panel_sort) {
        let Some(agent) = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
        else {
            continue;
        };
        if agent.board.asks.is_empty() {
            continue;
        }
        lines.push(agent_heading(snapshot, agent, config));
        for ask in &agent.board.asks {
            let (mark, color) = ask_mark(ask.kind, config);
            lines.push(BoardLine {
                spans: vec![
                    Span::raw("   "),
                    Span::styled(format!("{mark} "), Style::default().fg(color)),
                    Span::styled(ask.text.clone(), Style::default().fg(config.palette.text)),
                ],
                target: Some(BoardTarget::FocusPane(agent.pane_id.clone())),
            });
        }
    }
    lines
}

/// One agent's whole board, wrapped to the column width.
fn column_lines(board: &PaneBoard, width: u16, config: &ClientShellConfig) -> Vec<BoardLine> {
    let palette = &config.palette;
    // " " + mark + " " before the text, and the same indent on later rows.
    let text_width = width.saturating_sub(3).max(1);
    let mut lines = Vec::new();
    if !board.asks.is_empty() {
        lines.push(section("あなたへ", None, config));
        for ask in &board.asks {
            let (mark, color) = ask_mark(ask.kind, config);
            lines.extend(item(
                (mark, Style::default().fg(color)),
                &ask.text,
                Style::default().fg(palette.text),
                text_width,
                None,
            ));
        }
        lines.push(blank());
    }
    if !board.links.is_empty() {
        lines.push(section("リンク", None, config));
        for link in &board.links {
            let target = Some(BoardTarget::OpenUrl(link.url.clone()));
            lines.extend(item(
                ("↗", Style::default().fg(palette.accent)),
                &link.label,
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::UNDERLINED),
                text_width,
                target.clone(),
            ));
            let url = link
                .url
                .split_once("://")
                .map_or(link.url.as_str(), |(_, rest)| rest);
            lines.push(BoardLine {
                spans: vec![
                    Span::raw("   "),
                    Span::styled(
                        truncate(url, text_width),
                        Style::default().fg(palette.overlay0),
                    ),
                ],
                target,
            });
        }
        lines.push(blank());
    }
    if !board.todos.is_empty() {
        let finished = board
            .todos
            .iter()
            .filter(|todo| todo.state == TodoState::Done)
            .count();
        let count = format!("{finished}/{}", board.todos.len());
        lines.push(section("To-Do", Some(count), config));
        for todo in &board.todos {
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
            lines.extend(item(
                (mark, mark_style),
                &todo.text,
                text_style,
                text_width,
                None,
            ));
        }
    }
    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    lines
}

/// A marked entry, wrapped onto as many rows as it needs.
fn item(
    (mark, mark_style): (&str, Style),
    text: &str,
    text_style: Style,
    width: u16,
    target: Option<BoardTarget>,
) -> Vec<BoardLine> {
    wrap(text, width)
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let lead = if index == 0 {
                Span::styled(format!(" {mark} "), mark_style)
            } else {
                Span::raw("   ")
            };
            BoardLine {
                spans: vec![lead, Span::styled(row, text_style)],
                target: target.clone(),
            }
        })
        .collect()
}

fn section(title: &str, count: Option<String>, config: &ClientShellConfig) -> BoardLine {
    let mut spans = vec![Span::styled(format!(" {title}"), section_style(config))];
    if let Some(count) = count {
        spans.push(Span::styled(
            format!(" {count}"),
            Style::default().fg(config.palette.overlay0),
        ));
    }
    BoardLine {
        spans,
        target: None,
    }
}

fn blank() -> BoardLine {
    BoardLine {
        spans: Vec::new(),
        target: None,
    }
}

fn section_style(config: &ClientShellConfig) -> Style {
    Style::default()
        .fg(config.palette.overlay0)
        .add_modifier(Modifier::BOLD)
}

fn ask_mark(kind: AskKind, config: &ClientShellConfig) -> (&'static str, ratatui::style::Color) {
    match kind {
        AskKind::Do => ("!", config.palette.peach),
        _ => ("?", config.palette.red),
    }
}

/// Breaks text into rows of at most `width` display columns, preferring
/// spaces and otherwise breaking anywhere (Japanese has no spaces).
fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if row_width + ch_width > width {
            let carried = match row.rfind(' ') {
                Some(space) if space > 0 && ch != ' ' => {
                    let rest = row[space + 1..].to_string();
                    row.truncate(space);
                    rest
                }
                _ => String::new(),
            };
            rows.push(std::mem::take(&mut row));
            row_width = display_width(&carried) as usize;
            row = carried;
            if ch == ' ' {
                continue;
            }
        }
        row.push(ch);
        row_width += ch_width;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn truncate(text: &str, width: u16) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width + 1 > usize::from(width) {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

fn agent_heading(
    snapshot: &ClientShellSnapshot,
    agent: &ClientShellAgent,
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

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wrap_prefers_spaces_and_breaks_japanese_anywhere() {
        assert_eq!(wrap("run the tests", 8), vec!["run the", "tests"]);
        assert_eq!(wrap("あいうえお", 4), vec!["あい", "うえ", "お"]);
        assert_eq!(wrap("short", 10), vec!["short"]);
    }
}
